// Polaris 飞书对话引擎 · Node 桥
// 用飞书官方 SDK 的 WSClient 起「长连接」，收 im.message.receive_v1 事件 → 打到 stdout(JSON 行)；
// 从 stdin 读 {type:'reply'} 指令 → 调 im.message.create 把 Claude 的回复发回飞书。
// Rust 端(feishu.rs 网关)负责把消息路由给 headless claude 再把回复写回本进程 stdin。
//
// 自愈三件套（在不破坏上面 stdout 协议的前提下叠加）：
//   ① 父亡即自尽：父进程(polaris-app)死 → stdin 管道 EOF / ppid 变化 → 自己退出，杜绝孤儿空转烧 CPU、重复占飞书长连接。
//   ② 落盘日志：除 stdout 协议外，再把 启动/连上/断线/重连/收发/回调失败 追加到 logs/bridge-YYYYMMDD.log(NDJSON)。
//   ③ 健康探针：心跳监控长连接状态，掉线超阈值 → exit(1) 让 Rust 监工重起一个干净进程。
import * as Lark from "@larksuiteoapi/node-sdk";
import { appendFileSync, mkdirSync, readdirSync, statSync, unlinkSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// ───────────────────────── 调参 ─────────────────────────
const HEARTBEAT_MS = 30_000; // 心跳间隔
const HEALTH_HARD_MS = 90_000; // 掉线超此值 → 主动退出让监工重起
const PARENT_POLL_MS = 5_000; // ppid 轮询间隔（父亡兜底）
const LOG_RETENTION_DAYS = 14; // 日志保留天数

const appId = process.env.FEISHU_APP_ID || "";
const appSecret = process.env.FEISHU_APP_SECRET || "";
const isLark = (process.env.FEISHU_DOMAIN || "feishu") === "lark";

// ───────────────────────── stdout 协议（Rust 端在解析，勿改格式）─────────────────────────
function send(obj) {
  try {
    process.stdout.write(JSON.stringify(obj) + "\n");
  } catch {
    /* ignore */
  }
}

// ───────────────────────── 落盘日志（NDJSON，附加层，不影响 stdout 协议）─────────────────────────
const LOG_DIR = join(dirname(fileURLToPath(import.meta.url)), "logs");
function logFilePath() {
  const d = new Date();
  const ymd = `${d.getFullYear()}${String(d.getMonth() + 1).padStart(2, "0")}${String(
    d.getDate()
  ).padStart(2, "0")}`;
  return join(LOG_DIR, `bridge-${ymd}.log`);
}
function logEvent(level, event, fields = {}) {
  const rec = { ts: new Date().toISOString(), pid: process.pid, level, event, ...fields };
  try {
    appendFileSync(logFilePath(), JSON.stringify(rec) + "\n");
  } catch {
    /* 日志不可写绝不拖垮主流程 */
  }
}
function pruneOldLogs() {
  try {
    const now = Date.now();
    for (const f of readdirSync(LOG_DIR)) {
      if (!/^bridge-\d{8}\.log$/.test(f)) continue;
      const p = join(LOG_DIR, f);
      if ((now - statSync(p).mtimeMs) / 86_400_000 > LOG_RETENTION_DAYS) unlinkSync(p);
    }
  } catch {
    /* ignore */
  }
}
try {
  mkdirSync(LOG_DIR, { recursive: true });
} catch {
  /* ignore */
}
pruneOldLogs();

logEvent("info", "start", {
  ppid: process.ppid,
  domain: isLark ? "lark" : "feishu",
  node: process.version,
});

if (!appId || !appSecret) {
  logEvent("error", "fatal_no_credential");
  send({ type: "fatal", text: "缺少 App ID / App Secret" });
  process.exit(1);
}

// ───────────────────────── 优雅退出（一次性闸门）─────────────────────────
let shuttingDown = false;
function shutdown(reason, code = 0) {
  if (shuttingDown) return;
  shuttingDown = true;
  logEvent(code === 0 ? "warn" : "error", "shutdown", { reason, code });
  send({ type: "log", text: `桥退出(${reason})` });
  try {
    if (typeof wsClient?.stop === "function") wsClient.stop();
  } catch {
    /* SDK 未必有 stop，进程退出即清理 */
  }
  // 给日志/stdout 落地一拍再退
  setTimeout(() => process.exit(code), 50);
}

// ───────────────────────── 健康状态机 ─────────────────────────
let connState = "starting";
let downSince = Date.now(); // 尚未连上即视为「下线中」，首连超时也会触发自重起
function markConnected(event) {
  connState = "connected";
  downSince = null;
  send({ type: "status", state: "connected" });
  logEvent("info", event);
}
function markDown(state, event) {
  connState = state;
  if (downSince === null) downSince = Date.now();
  if (state === "reconnecting") send({ type: "status", state: "reconnecting" });
  logEvent("warn", event, {});
}

const baseCfg = { appId, appSecret };
if (isLark) baseCfg.domain = Lark.Domain.Lark;

const client = new Lark.Client(baseCfg);
// autoReconnect + 回调：WS 断线官方 SDK 自动重连，并把状态回传给 Rust 端（防断 + 自检）。
const wsClient = new Lark.WSClient({
  ...baseCfg,
  autoReconnect: true,
  onReady: () => markConnected("ws_connected"),
  onError: (e) => {
    logEvent("error", "ws_error", { msg: String((e && e.message) || e) });
    send({ type: "log", text: "连接错误: " + ((e && e.message) || e) });
  },
  onReconnecting: () => markDown("reconnecting", "ws_reconnecting"),
  onReconnected: () => markConnected("ws_reconnected"),
  onClose: () => markDown("closed", "ws_close"),
});

// ───────────────────────── 心跳健康探针 ─────────────────────────
setInterval(() => {
  const downMs = downSince === null ? 0 : Date.now() - downSince;
  logEvent("debug", "heartbeat", { state: connState, downSec: Math.round(downMs / 1000) });
  if (connState !== "connected" && downMs > HEALTH_HARD_MS) {
    logEvent("error", "health_timeout_exit", { downSec: Math.round(downMs / 1000) });
    send({ type: "log", text: `长连接掉线超 ${Math.round(downMs / 1000)}s，主动退出由监工重起。` });
    shutdown("health_timeout", 1);
  }
}, HEARTBEAT_MS);

// ───────────────────────── 父亡即自尽 ─────────────────────────
// 父进程(polaris-app)正常派生时，stdin 是它喂进来的管道：父进程一死 → 管道 EOF → end/close 触发。
process.stdin.setEncoding("utf8");
process.stdin.on("end", () => shutdown("parent_stdin_end"));
process.stdin.on("close", () => shutdown("parent_stdin_close"));
process.stdin.on("error", (e) => {
  logEvent("warn", "stdin_error", { msg: String((e && e.message) || e) });
  shutdown("parent_stdin_error");
});
// 兜底：Windows 上父进程被强杀后子进程 ppid 会变（被 reparent/失效），轮询自检。
const bornPpid = process.ppid;
setInterval(() => {
  if (process.ppid !== bornPpid) shutdown("parent_ppid_changed");
}, PARENT_POLL_MS);
// 信号：被显式终止时也走优雅退出，留日志。
for (const sig of ["SIGINT", "SIGTERM", "SIGHUP", "SIGBREAK"]) {
  try {
    process.on(sig, () => shutdown(`signal_${sig}`));
  } catch {
    /* 平台不支持该信号则跳过 */
  }
}

// ───────────────────────── stdin: 逐行读回复指令 {type:'reply', chatId, text} ─────────────────────────
let buf = "";
process.stdin.on("data", async (chunk) => {
  buf += chunk;
  let idx;
  while ((idx = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, idx).trim();
    buf = buf.slice(idx + 1);
    if (!line) continue;
    let msg = null;
    try {
      msg = JSON.parse(line);
      if (msg.type === "reply" && msg.chatId && msg.text) {
        await client.im.v1.message.create({
          params: { receive_id_type: "chat_id" },
          data: {
            receive_id: msg.chatId,
            msg_type: "text",
            content: JSON.stringify({ text: msg.text }),
          },
        });
        logEvent("info", "reply_sent", { chatId: msg.chatId, len: String(msg.text).length });
        send({ type: "log", text: "已回复 " + msg.chatId });
      }
    } catch (e) {
      logEvent("error", "reply_failed", {
        chatId: (msg && msg.chatId) || "",
        msg: String((e && e.message) || e),
      });
      send({ type: "log", text: "回复失败: " + ((e && e.message) || e) });
    }
  }
});

// ───────────────────────── WS: 收消息 → stdout(JSON) 交给 Rust 路由 ─────────────────────────
wsClient.start({
  eventDispatcher: new Lark.EventDispatcher({}).register({
    "im.message.receive_v1": async (data) => {
      try {
        const m = (data && data.message) || {};
        let text = "";
        try {
          text = JSON.parse(m.content || "{}").text || "";
        } catch {
          /* 非文本消息 */
        }
        // 去掉 @机器人 占位符（飞书把 @ 渲染成 @_user_1 之类）
        text = text.replace(/@_user_\d+/g, "").trim();
        const chatId = m.chat_id || "";
        const messageId = m.message_id || "";
        const chatType = m.chat_type || "p2p";
        const mentioned = Array.isArray(m.mentions) && m.mentions.length > 0;
        // 日志只记元数据(id/类型/长度)，不落消息正文，防隐私/敏感内容入盘。
        logEvent("info", "message_in", { chatId, messageId, chatType, mentioned, len: text.length });
        send({
          type: "message",
          chatId,
          messageId,
          chatType,
          mentioned,
          senderOpenId:
            (data.sender && data.sender.sender_id && data.sender.sender_id.open_id) || "",
          text,
        });
      } catch (e) {
        logEvent("error", "message_parse_failed", { msg: String((e && e.message) || e) });
        send({ type: "log", text: "收消息解析失败: " + ((e && e.message) || e) });
      }
    },
  }),
});

send({ type: "log", text: "长连接启动中…" });
process.on("uncaughtException", (e) => {
  logEvent("error", "uncaught_exception", { msg: String((e && e.message) || e) });
  send({ type: "log", text: "未捕获异常: " + ((e && e.message) || e) });
});
process.on("unhandledRejection", (e) => {
  logEvent("error", "unhandled_rejection", { msg: String((e && e.message) || e) });
});
