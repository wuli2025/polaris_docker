# -*- coding: utf-8 -*-
"""
微信聊天 · 每日待办提取（自动化流程每天跑一次）
─────────────────────────────────────────────────────────────
做三件事：
  1. 用已缓存的 master key 解密 + 导出当前微信库（复用 wechat-decrypt 工具链）。
  2. 从导出里挖「待办」：只看**你回过话**的会话（私聊 + 你活跃的群），
     取**最近 N 天别人发来、你还没回**的消息 → 一条待办。
  3. 把待办写进 Polaris 晨报（kb_root/memory/briefing/<今天>.json），
     在对话框/自动化页顶部以晨报卡片呈现，点「让我去做」即起一轮对话帮你回。

纯标准库；解密/导出步骤 shell 出到 wechat-decrypt 的 venv 跑（那边有 pycryptodome 等）。
配置读同目录 wx_config.json（首次用 wx_setup.py 生成、缓存 master key）。

用法：  python wx_daily.py            # 跑全流程（解密→导出→挖待办→写晨报）
        python wx_daily.py --no-export   # 跳过解密导出，只对已有导出挖待办（调试用）
"""
import os, sys, json, time, hashlib, hmac, struct, subprocess, hashlib as _h

HERE = os.path.dirname(os.path.abspath(__file__))
PAGE = 4096


def log(*a):
    print(*a); sys.stdout.flush()


def load_config():
    p = os.path.join(HERE, "wx_config.json")
    if not os.path.exists(p):
        log("[!] 缺少 wx_config.json —— 请先运行  python wx_setup.py  完成一次性授权。")
        sys.exit(2)
    with open(p, encoding="utf-8") as f:
        return json.load(f)


# ───────────────────────── kb_root 解析（写晨报用）─────────────────────────
def resolve_kb_root():
    appdata = os.environ.get("APPDATA", "")
    settings = os.path.join(appdata, "polaris", "polaris-app", "config", "settings.json")
    try:
        with open(settings, encoding="utf-8") as f:
            kr = json.load(f).get("kb_root")
            if kr and os.path.isdir(kr):
                return kr
    except Exception:
        pass
    return os.path.join(os.environ.get("USERPROFILE", os.path.expanduser("~")), "Polaris", "PolarisKB")


# ───────────────────────── master key 有效性校验（纯标准库）─────────────────────────
def verify_enc(enc, page1):
    salt = page1[:16]
    mac_salt = bytes(b ^ 0x3A for b in salt)
    mac_key = hashlib.pbkdf2_hmac("sha512", enc, mac_salt, 2, dklen=32)
    data = page1[16: PAGE - 80 + 16]
    stored = page1[PAGE - 64: PAGE]
    hm = hmac.new(mac_key, data, hashlib.sha512); hm.update(struct.pack("<I", 1))
    return hm.digest() == stored


def master_still_valid(cfg):
    db = os.path.join(cfg["db_dir"], "message", "message_0.db")
    try:
        page1 = open(db, "rb").read(PAGE)
    except Exception as e:
        log(f"[!] 读不到微信库 message_0.db：{e}")
        return False
    try:
        master = bytes.fromhex(cfg["master_key"])
    except Exception:
        return False
    salt = page1[:16]
    enc = hashlib.pbkdf2_hmac("sha512", master, salt, 256000, dklen=32)
    return verify_enc(enc, page1)


# ───────────────────────── 解密 + 导出（shell 到 wechat-decrypt venv）─────────────────────────
def run_pipeline(cfg):
    venv = cfg["venv_python"]
    tools = cfg["tools_dir"]
    env = dict(os.environ); env["PYTHONUTF8"] = "1"; env["PYTHONIOENCODING"] = "utf-8"
    steps = [
        ("派生全库 key", [venv, "derive_all_keys.py", cfg["master_key"]]),
        ("解密全部库", [venv, "decrypt_db.py"]),
        ("导出全部聊天", [venv, "export_all_chats.py"]),
    ]
    for name, cmd in steps:
        log(f"  → {name} …")
        r = subprocess.run(cmd, cwd=tools, env=env, capture_output=True, text=True, encoding="utf-8", errors="replace")
        if r.returncode != 0:
            tail = (r.stdout or "")[-600:] + (r.stderr or "")[-600:]
            raise RuntimeError(f"{name} 失败（exit {r.returncode}）：\n{tail}")
    log("  ✓ 解密 + 导出完成")


# ───────────────────────── 待办挖掘 ─────────────────────────
def is_official(username):
    return username.startswith("gh_")


def my_msg(m):
    return m.get("sender") == "me"


def incoming(m):
    s = m.get("sender", "")
    return s and s != "me" and m.get("type") != "system"


def msg_text(m):
    t = m.get("content") or ""
    if not t:
        ty = m.get("type", "")
        t = {"image": "[图片]", "sticker": "[表情]", "video": "[视频]", "voice": "[语音]",
             "location": "[位置]", "transfer": "[转账]", "contact_card": "[名片]",
             "call": "[通话]", "link_or_file": "[链接/文件]"}.get(ty, "")
    return t.replace("\n", " ").strip()


def at_me(text, nicks):
    return any(("@" + n) in text for n in nicks if n)


# 纯寒暄/收尾的应答——不构成「待办」。文件/图片/语音等附件不算寒暄（可能是要处理的东西）。
_TRIVIAL_WORDS = {
    "好", "好的", "好滴", "好呀", "好嘞", "行", "行吧", "可以", "嗯", "嗯嗯", "嗯呐", "哦",
    "收到", "知道了", "了解", "谢谢", "感谢", "多谢", "辛苦了", "辛苦", "ok", "okk", "okay",
    "没问题", "👌", "👍", "在", "哈哈", "哈哈哈", "哈哈哈哈",
}
_NONTRIVIAL_TAGS = ("[文件]", "[图片]", "[视频]", "[语音]", "[位置]", "[转账]", "[名片]", "[链接/文件]")


def is_trivial(m):
    raw = (m.get("content") or "").strip()
    ty = m.get("type", "")
    if ty in ("image", "video", "voice", "location", "transfer", "contact_card", "link_or_file"):
        return False  # 附件类一律保留
    t = msg_text(m)
    if any(tag in t for tag in _NONTRIVIAL_TAGS):
        return False
    # 去掉 [xxx] 表情标记与标点后，若空 或 落在寒暄词表里 → 寒暄
    import re
    bare = re.sub(r"\[[^\]]{1,6}\]", "", raw)
    bare = re.sub(r"[\s，。!！？?~、…\.]+", "", bare).lower()
    if not bare:
        return True  # 纯表情/纯标点
    return bare in _TRIVIAL_WORDS


def extract_tasks(cfg):
    src = cfg["exported_dir"]
    nicks = cfg.get("my_nicks", [])
    window = float(cfg.get("window_days", 7)) * 86400
    now = time.time()
    floor = now - window
    cands = []

    for fn in os.listdir(src):
        if not fn.endswith(".json") or fn == "_export_index.json":
            continue
        try:
            j = json.load(open(os.path.join(src, fn), encoding="utf-8"))
        except Exception:
            continue
        username = j.get("username", "")
        if is_official(username) or username in ("filehelper", "weixin", "newsapp"):
            continue
        is_group = bool(j.get("is_group"))
        name = j.get("chat") or username
        msgs = sorted(j.get("messages", []), key=lambda m: m.get("timestamp", 0))
        if not msgs:
            continue

        # 「你回过话」才算——挑出你参与过的会话（私聊/活跃群的核心判据）
        my_ts = [m["timestamp"] for m in msgs if my_msg(m) and m.get("timestamp")]
        if not my_ts:
            continue
        last_me = max(my_ts)

        # 群：要求你近 30 天内说过话才算「活跃群」，否则跳过
        if is_group and last_me < now - 30 * 86400:
            continue

        # 最近 N 天、别人发来、且在你最后一次发言之后（=还没回）的消息
        pending = [m for m in msgs
                   if incoming(m) and m.get("timestamp", 0) >= floor and m.get("timestamp", 0) > last_me]
        if not pending:
            continue
        # 全是寒暄/收尾应答（好/嗯/谢谢/纯表情）→ 不算待办
        if all(is_trivial(m) for m in pending):
            continue

        # 群里进一步聚焦：优先 @你 的；没人 @你的群消息只在你刚聊过(48h内)时留，避免灌水噪音
        mentioned = any(at_me(msg_text(m), nicks) for m in pending)
        if is_group and not mentioned and last_me < now - 2 * 86400:
            continue

        last_in = max(pending, key=lambda m: m.get("timestamp", 0))
        # 排序分：私聊 > 群@你 > 活跃群；同档按最新来信时间
        if not is_group:
            base = 300
        elif mentioned:
            base = 200
        else:
            base = 100
        score = base + last_in.get("timestamp", 0) / 1e9

        cands.append({
            "username": username, "name": name, "is_group": is_group,
            "mentioned": mentioned, "pending": pending, "last_in_ts": last_in.get("timestamp", 0),
            "score": score,
        })

    cands.sort(key=lambda c: c["score"], reverse=True)
    return cands[: int(cfg.get("max_tasks", 8))]


def fmt_day(ts):
    return time.strftime("%m-%d %H:%M", time.localtime(ts))


def to_suggestions(cands):
    out = []
    for c in cands:
        name16 = c["name"][:16]
        pend = c["pending"][-3:]  # 最多展示最近 3 条
        # 标题取最后一条**有内容**的来信（跳过寒暄）
        meaty = [m for m in c["pending"] if not is_trivial(m)] or c["pending"]
        last_txt = msg_text(meaty[-1])[:24] or "（非文字消息）"
        if c["is_group"]:
            who = "群「" + name16 + "」"
            tag = "有人@你，" if c["mentioned"] else ""
            title = f"回应{who}：{tag}{last_txt}"
            why = f"{who}里 {fmt_day(c['last_in_ts'])} {'有人@你并' if c['mentioned'] else ''}发了消息，你最近在这个群说过话但还没回这条。"
            action_who = f"微信群「{c['name']}」"
        else:
            title = f"回复 {name16}：{last_txt}"
            why = f"「{c['name']}」在 {fmt_day(c['last_in_ts'])} 给你发了消息，你们最近聊过、但你还没回。"
            action_who = f"微信好友「{c['name']}」"
        lines = "\n".join(f"  · {fmt_day(m.get('timestamp', 0))} {(m.get('sender') or '')[:10]}：{msg_text(m)[:60]}" for m in pend)
        how = f"待你回应的消息：\n{lines}"
        action = (f"我在 {action_who} 收到这些还没回的消息：\n{lines}\n"
                  f"帮我判断这是否需要回复，如需要就拟 2 条得体、贴合我语气的回复草稿让我挑；"
                  f"如果只是寒暄/无需回，就告诉我可以忽略。")
        # 稳定 id：同一会话+同一最后来信时间→同一条，re-run 不重复且可保留 dismissed
        sid = "wx-" + _h.md5(f"{c['username']}|{c['last_in_ts']}".encode("utf-8")).hexdigest()[:10]
        out.append({
            "id": sid, "title": title, "kind": "progress",
            "source": name16, "why": why, "how": how, "action": action,
            "dismissed": False,
        })
    return out


def write_briefing(kb_root, sugs):
    day = time.strftime("%Y-%m-%d", time.localtime())
    bdir = os.path.join(kb_root, "memory", "briefing")
    os.makedirs(bdir, exist_ok=True)
    path = os.path.join(bdir, f"{day}.json")
    existing = []
    try:
        with open(path, encoding="utf-8") as f:
            existing = json.load(f)
    except Exception:
        existing = []
    # 保留被 dismiss 的旧微信待办状态；去掉旧的微信条目后重铺
    dismissed_ids = {s["id"] for s in existing if isinstance(s, dict) and s.get("id", "").startswith("wx-") and s.get("dismissed")}
    kept = [s for s in existing if not (isinstance(s, dict) and s.get("id", "").startswith("wx-"))]
    for s in sugs:
        if s["id"] in dismissed_ids:
            s["dismissed"] = True
    merged = kept + sugs
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(merged, f, ensure_ascii=False, indent=2)
    os.replace(tmp, path)
    return path


def main():
    cfg = load_config()
    no_export = "--no-export" in sys.argv

    if not no_export:
        if not master_still_valid(cfg):
            # key 失效（多半是重新登录过）——给一条「重新授权」的待办，别静默失败
            kb = resolve_kb_root()
            p = write_briefing(kb, [{
                "id": "wx-reauth", "title": "微信授权已过期，请重新授权",
                "kind": "organize", "source": "微信待办",
                "why": "缓存的微信密钥对当前数据库校验失败（通常是微信重新登录过）。",
                "how": "在技能中心打开「微信聊天 · 每日待办」，或运行 wx_setup.py 重新抓取一次密钥（会重启微信让你扫码登录）。",
                "action": "微信每日待办的密钥失效了，请指导我重新运行 wx_setup.py 完成授权。",
                "dismissed": False,
            }])
            log(f"[!] master key 失效，已写「重新授权」提醒 → {p}")
            sys.exit(3)
        log("[1/3] 密钥有效，开始解密 + 导出当前微信库 …")
        run_pipeline(cfg)
    else:
        log("[1/3] 跳过解密导出（--no-export）")

    log("[2/3] 从导出里挖待办（私聊 + 活跃群，最近 %s 天别人发来未回）…" % cfg.get("window_days", 7))
    cands = extract_tasks(cfg)
    sugs = to_suggestions(cands)
    log(f"      命中 {len(sugs)} 条待办")
    for s in sugs:
        log("       - " + s["title"])

    kb = resolve_kb_root()
    log(f"[3/3] 写入晨报 kb_root={kb}")
    p = write_briefing(kb, sugs)
    log(f"      ✓ 已写 {p}")
    log(f"\n完成：{len(sugs)} 条微信待办已进晨报，去对话框/自动化页顶部查看。")


if __name__ == "__main__":
    main()
