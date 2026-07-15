<script setup lang="ts">
// 语音输入「极速说」设置页(设置 → 语音输入)
// PRD v3 §8:激活方式 / 识别引擎 / 防污染档位(秒达·重型)/ 流畅模式 / 润色 /
// 个人词表(热词·同音纠错)/ 防污染「测一下」/ 词表自学。
// 后端见 src-tauri/src/voice.rs;采集/注入/推理运行时为下一阶段,本页只配置与防污染。
import { onMounted, ref, computed } from "vue";
import { invoke } from "../tauri";
import { useAppStore } from "../stores/app";

defineOptions({ name: "VoiceSettings" });

const app = useAppStore();

interface VoiceConfig {
  activation: string;
  hotkey: string;
  engine: string;
  fluent_mode: boolean;
  polish: boolean;
  antipollute: string;
  pinyin_threshold: number;
  overlay_pos: string;
  polish_api_base: string;
  polish_api_key: string;
  polish_model: string;
}
interface PolishResult {
  raw: string;
  text: string;
  model: string;
  ms: number;
  key_source: string;
}
interface VoiceLexicon {
  hotwords: string[];
  corrections: Record<string, string>;
  weights: Record<string, number>;
}
interface AntiChange {
  from: string;
  to: string;
  layer: string;
}
interface AntiPolluteResult {
  text: string;
  changes: AntiChange[];
  tier: string;
  heavy_pending: boolean;
}
interface MinedTerm {
  term: string;
  count: number;
}
interface TranscribeResult {
  raw: string;
  text: string;
  changes: AntiChange[];
  tier: string;
  ms: number;
}

const cfg = ref<VoiceConfig | null>(null);
const lex = ref<VoiceLexicon | null>(null);
const loadErr = ref("");

// 已知本地引擎(展示用;完整清单在感官 API 坞)
const ENGINES = [
  { id: "local-sensevoice", name: "SenseVoice-Small（本地·默认·秒达模拟流式）" },
  { id: "local-paraformer-stream", name: "Paraformer 流式（流畅模式·真增量）" },
  { id: "local-paraformer", name: "Paraformer-zh（字级时间戳）" },
  { id: "siliconflow-asr", name: "硅基流动 SenseVoice（云·免费·无流式）" },
  { id: "tencent-asr", name: "腾讯云实时识别（云·每月 5h 免费）" },
];
const HOTKEYS = [
  { id: "ralt", name: "右 Alt（默认）" },
  { id: "rctrl", name: "右 Ctrl" },
  { id: "capslock", name: "CapsLock" },
  { id: "f9", name: "F9" },
  { id: "mouse_x2", name: "鼠标侧键 ×2" },
];

async function refresh() {
  try {
    cfg.value = await invoke<VoiceConfig>("voice_config_get");
    lex.value = await invoke<VoiceLexicon>("voice_lexicon_get");
    loadErr.value = "";
  } catch (e) {
    loadErr.value = String(e);
  }
}

async function setCfg(patch: Record<string, unknown>) {
  try {
    cfg.value = await invoke<VoiceConfig>("voice_config_set", patch);
  } catch (e) {
    loadErr.value = String(e);
  }
}

onMounted(refresh);

// ── 防污染「测一下」──
const probeIn = ref("把设置改成扣带式那种形态，名字就叫北极心吧");
const probeRes = ref<AntiPolluteResult | null>(null);
const probeBusy = ref(false);
async function runProbe() {
  if (probeBusy.value) return;
  probeBusy.value = true;
  probeRes.value = null;
  try {
    probeRes.value = await invoke<AntiPolluteResult>("voice_anti_pollute", {
      text: probeIn.value,
    });
  } catch (e) {
    loadErr.value = String(e);
  } finally {
    probeBusy.value = false;
  }
}

// ── AI 整形（仿 Typeless）测一下 ──
const polishIn = ref("嗯……那个我们呢就是说先把这个北极星的方案确定下来然后然后再排期不对是先排期");
const polishRes = ref<PolishResult | null>(null);
const polishErr = ref("");
const polishBusy = ref(false);
async function runPolish() {
  if (polishBusy.value) return;
  polishBusy.value = true;
  polishRes.value = null;
  polishErr.value = "";
  try {
    polishRes.value = await invoke<PolishResult>("voice_polish", { text: polishIn.value });
  } catch (e) {
    polishErr.value = String(e);
  } finally {
    polishBusy.value = false;
  }
}
const keySourceLabel = (s: string) =>
  s === "config" ? "你填的 Key" : s === "borrowed" ? "借用坞里 MiniMax" : "无 Key";

// ── 启用实时语音输入（按住热键说话 → 注入焦点应用）──
const listenOn = ref(false);
const listenErr = ref("");
async function toggleListen() {
  listenErr.value = "";
  try {
    if (!listenOn.value) {
      await invoke("voice_listen_start");
      listenOn.value = true;
    } else {
      await invoke("voice_listen_stop");
      listenOn.value = false;
    }
  } catch (e) {
    listenErr.value = String(e);
    listenOn.value = false;
  }
}

// ── 测试识别（选音频文件，无需麦克风）──
const asrRes = ref<TranscribeResult | null>(null);
const asrErr = ref("");
const asrBusy = ref(false);
async function pickAndTranscribe() {
  if (asrBusy.value) return;
  asrErr.value = "";
  let path: string | null = null;
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      multiple: false,
      filters: [{ name: "音频（16k 单声道 wav）", extensions: ["wav"] }],
    });
    if (typeof picked === "string") path = picked;
  } catch (e) {
    asrErr.value = String(e);
    return;
  }
  if (!path) return;
  asrBusy.value = true;
  asrRes.value = null;
  try {
    asrRes.value = await invoke<TranscribeResult>("voice_transcribe_file", { path });
  } catch (e) {
    asrErr.value = String(e);
  } finally {
    asrBusy.value = false;
  }
}

// ── 热词 ──
const newHotword = ref("");
async function addHotword() {
  const w = newHotword.value.trim();
  if (!w) return;
  try {
    lex.value = await invoke<VoiceLexicon>("voice_hotword_add", { word: w });
    newHotword.value = "";
  } catch (e) {
    loadErr.value = String(e);
  }
}
async function removeHotword(w: string) {
  try {
    lex.value = await invoke<VoiceLexicon>("voice_hotword_remove", { word: w });
  } catch (e) {
    loadErr.value = String(e);
  }
}

// ── 纠错映射 ──
const newWrong = ref("");
const newRight = ref("");
async function addCorrection() {
  const wrong = newWrong.value.trim();
  const right = newRight.value.trim();
  if (!wrong || !right) return;
  try {
    lex.value = await invoke<VoiceLexicon>("voice_correction_add", { wrong, right });
    newWrong.value = "";
    newRight.value = "";
  } catch (e) {
    loadErr.value = String(e);
  }
}
async function removeCorrection(wrong: string) {
  try {
    lex.value = await invoke<VoiceLexicon>("voice_correction_remove", { wrong });
  } catch (e) {
    loadErr.value = String(e);
  }
}

const correctionList = computed(() =>
  Object.entries(lex.value?.corrections ?? {}).map(([wrong, right]) => ({ wrong, right }))
);

// ── 词表自学 ──
const learnIn = ref("");
const mined = ref<MinedTerm[]>([]);
const learnBusy = ref(false);
async function runLearn() {
  if (learnBusy.value || !learnIn.value.trim()) return;
  learnBusy.value = true;
  try {
    mined.value = await invoke<MinedTerm[]>("voice_lexicon_learn", {
      text: learnIn.value,
      top: 20,
    });
    lex.value = await invoke<VoiceLexicon>("voice_lexicon_get");
  } catch (e) {
    loadErr.value = String(e);
  } finally {
    learnBusy.value = false;
  }
}
</script>

<template>
  <div class="voice">
    <header class="head">
      <div>
        <h1>语音输入「极速说」</h1>
        <p class="sub">
          按住右 Alt 说话、字随声出、松手上屏。默认本地 SenseVoice + 秒达防污染（热词偏置 + 拼音纠同音），纯本地零出域。
          <span class="dim">采集/热键/注入/推理运行时为下一阶段，本页先把配置与防污染打磨好。</span>
        </p>
      </div>
      <button class="btn" @click="app.setView('settings')">← 返回设置</button>
    </header>

    <div v-if="loadErr" class="err-line">{{ loadErr }}</div>

    <template v-if="cfg">
      <!-- 启用实时语音输入 -->
      <section class="block">
        <div class="b-head">
          <h2>实时语音输入</h2>
          <span class="b-desc">
            启用后:按住
            <b>{{ HOTKEYS.find((k) => k.id === cfg?.hotkey)?.name || cfg?.hotkey }}</b>
            说话 → 浮窗流式上字 → 松手识别 + 防污染 → 自动敲进当前焦点应用
          </span>
        </div>
        <div class="row">
          <button class="btn primary" :class="{ on: listenOn }" @click="toggleListen">
            {{ listenOn ? "■ 停用语音输入" : "● 启用语音输入" }}
          </button>
          <span v-if="listenOn" class="dim sm">已就绪 — 按住 {{ cfg.hotkey }} 说话</span>
          <span v-if="listenErr" class="err-line" style="margin: 0">{{ listenErr }}</span>
        </div>
        <div class="tierhint">
          Windows 桌面版已内置本地语音识别;首次使用前请在「设置 → 感官 API」
          下载「SenseVoice-Small」感官包(约 230&nbsp;MB)。macOS 即将支持;Docker / Web 版暂不支持本地听写。
        </div>
      </section>

      <!-- 激活方式 -->
      <section class="block">
        <div class="b-head"><h2>激活方式</h2></div>
        <div class="row">
          <div class="seg">
            <button :class="{ on: cfg.activation === 'hold' }" @click="setCfg({ activation: 'hold' })">
              按住说
            </button>
            <button :class="{ on: cfg.activation === 'free' }" @click="setCfg({ activation: 'free' })">
              自由说
            </button>
          </div>
          <label class="fl">
            激活键
            <select :value="cfg.hotkey" @change="setCfg({ hotkey: ($event.target as HTMLSelectElement).value })">
              <option v-for="k in HOTKEYS" :key="k.id" :value="k.id">{{ k.name }}</option>
            </select>
          </label>
          <span class="dim sm">Windows 右 Alt 可能 = AltGr，撞快捷键就改键</span>
        </div>
      </section>

      <!-- 识别引擎 -->
      <section class="block">
        <div class="b-head"><h2>识别引擎</h2></div>
        <div class="row">
          <select
            class="wide"
            :value="cfg.engine"
            @change="setCfg({ engine: ($event.target as HTMLSelectElement).value })"
          >
            <option v-for="e in ENGINES" :key="e.id" :value="e.id">{{ e.name }}</option>
          </select>
          <button class="btn sm" @click="app.setView('sense_api')">更多引擎（感官 API）›</button>
        </div>
        <div class="row">
          <label class="sw">
            <input
              type="checkbox"
              :checked="cfg.fluent_mode"
              @change="setCfg({ fluentMode: ($event.target as HTMLInputElement).checked })"
            />
            流畅模式（真流式 Paraformer，首次开启需下载 ~250MB）
          </label>
          <label class="sw">
            <input
              type="checkbox"
              :checked="cfg.polish"
              @change="setCfg({ polish: ($event.target as HTMLInputElement).checked })"
            />
            说完 AI 整形（仿 Typeless·去语气词/顺句/自动列表，走下方 API，默认关最快）
          </label>
        </div>
      </section>

      <!-- AI 整形（仿 Typeless）接入 -->
      <section class="block">
        <div class="b-head">
          <h2>AI 整形 · 接入便宜 API</h2>
          <span class="b-desc">
            开启上方「说完 AI 整形」后，松手/停录会把识别终稿发给这里配的 LLM，去语气词·去重复·补标点·顺句·自动列表——把「你说的话」变成「你想写的字」。走 OpenAI 兼容协议，几乎任何便宜 API 都能接。
          </span>
        </div>
        <div class="row">
          <label class="fl fill">
            接口 Base
            <input
              class="in"
              :value="cfg.polish_api_base"
              placeholder="https://api.minimaxi.com/v1"
              @change="setCfg({ polishApiBase: ($event.target as HTMLInputElement).value })"
            />
          </label>
        </div>
        <div class="row">
          <label class="fl fill">
            API Key
            <input
              class="in"
              type="password"
              :value="cfg.polish_api_key"
              placeholder="留空则自动借用「供应商坞」里的 MiniMax key（含粉丝福利额度）"
              @change="setCfg({ polishApiKey: ($event.target as HTMLInputElement).value })"
            />
          </label>
        </div>
        <div class="row">
          <label class="fl fill">
            模型
            <input
              class="in"
              :value="cfg.polish_model"
              placeholder="MiniMax-M2.7-highspeed"
              @change="setCfg({ polishModel: ($event.target as HTMLInputElement).value })"
            />
          </label>
        </div>
        <div class="tierhint">
          <b>优先推荐 MiniMax-M2.7-highspeed</b>：便宜、低延迟，整形这种短任务足够。国内用
          <code>https://api.minimaxi.com/v1</code>，海外用 <code>https://api.minimax.io/v1</code>。
          Key 留空即用内置的粉丝福利额度开箱即试；也可换成 DeepSeek / Kimi / 通义等任何 OpenAI 兼容便宜 API。
        </div>
        <textarea
          v-model="polishIn"
          class="ta"
          rows="2"
          placeholder="贴一段带语气词的口水话，看整形效果…"
        ></textarea>
        <div class="row">
          <button class="btn primary sm" :disabled="polishBusy" @click="runPolish">
            {{ polishBusy ? "整形中…" : "测一下整形" }}
          </button>
          <span v-if="polishRes" class="dim sm">
            {{ polishRes.model }} · {{ polishRes.ms }}ms · {{ keySourceLabel(polishRes.key_source) }}
          </span>
          <span v-if="polishErr" class="err-line" style="margin: 0">{{ polishErr }}</span>
        </div>
        <div v-if="polishRes" class="probe-out">
          <div class="dim sm">整形前（识别终稿）</div>
          <div class="po-text">{{ polishRes.raw }}</div>
          <div class="dim sm" style="margin-top: 8px">整形后</div>
          <div class="po-text">{{ polishRes.text }}</div>
        </div>
      </section>

      <!-- 防污染档位 -->
      <section class="block">
        <div class="b-head">
          <h2>防污染档位</h2>
          <span class="b-desc">中文识别 95% 的错是同音替换——把你的词表喂给它，比让模型更准更有效</span>
        </div>
        <div class="row">
          <div class="seg">
            <button :class="{ on: cfg.antipollute === 'lite' }" @click="setCfg({ antipollute: 'lite' })">
              ⚡ 秒达（默认）
            </button>
            <button :class="{ on: cfg.antipollute === 'heavy' }" @click="setCfg({ antipollute: 'heavy' })">
              🧠 重型
            </button>
            <button :class="{ on: cfg.antipollute === 'off' }" @click="setCfg({ antipollute: 'off' })">
              关闭
            </button>
          </div>
          <label class="fl">
            拼音阈值
            <select
              :value="cfg.pinyin_threshold"
              @change="setCfg({ pinyinThreshold: Number(($event.target as HTMLSelectElement).value) })"
            >
              <option :value="0">0（仅精确）</option>
              <option :value="1">1（推荐）</option>
              <option :value="2">2（激进）</option>
            </select>
          </label>
        </div>
        <div class="tierhint">
          <template v-if="cfg.antipollute === 'lite'">
            <b>⚡ 秒达</b>：热词偏置 + 拼音查表纠同音，<b>纯本地、无网络、~毫秒级</b>。治同音错（占错误大头）。
          </template>
          <template v-else-if="cfg.antipollute === 'heavy'">
            <b>🧠 重型</b>：秒达全部 + LLM 拼音/语义纠错（治语序/歧义）。
            <span class="warn">LLM 纠错待接入供应商坞，当前先跑秒达。</span>
          </template>
          <template v-else><b>关闭</b>：识别结果原样上屏，不做任何纠正。</template>
        </div>
      </section>

      <!-- 防污染「测一下」 -->
      <section class="block">
        <div class="b-head"><h2>测一下防污染</h2></div>
        <textarea v-model="probeIn" class="ta" rows="2" placeholder="贴一段识别原文，看防污染怎么纠…"></textarea>
        <div class="row">
          <button class="btn primary sm" :disabled="probeBusy" @click="runProbe">
            {{ probeBusy ? "纠错中…" : "运行防污染" }}
          </button>
          <span v-if="probeRes" class="dim sm">档位 {{ probeRes.tier }} · {{ probeRes.changes.length }} 处改动</span>
        </div>
        <div v-if="probeRes" class="probe-out">
          <div class="po-text">{{ probeRes.text }}</div>
          <div v-if="probeRes.changes.length" class="po-changes">
            <span v-for="(c, i) in probeRes.changes" :key="i" class="chg" :class="c.layer">
              <s>{{ c.from }}</s> → <b>{{ c.to }}</b>
              <em>{{ c.layer === "exact" ? "精确" : "拼音" }}</em>
            </span>
          </div>
          <div v-else class="dim sm">无改动（识别原文已干净，或没有命中词表）</div>
        </div>
      </section>

      <!-- 测试识别（选音频，无需麦克风） -->
      <section class="block">
        <div class="b-head">
          <h2>测试识别</h2>
          <span class="b-desc">选一个 16k 单声道 wav，跑本地 SenseVoice 识别 + 防污染（需以 voice-asr 构建并已下载模型）</span>
        </div>
        <div class="row">
          <button class="btn primary sm" :disabled="asrBusy" @click="pickAndTranscribe">
            {{ asrBusy ? "识别中…" : "选音频文件识别" }}
          </button>
          <span v-if="asrErr" class="err-line" style="margin: 0">{{ asrErr }}</span>
        </div>
        <div v-if="asrRes" class="probe-out">
          <div class="dim sm">原文（ASR）</div>
          <div class="po-text">{{ asrRes.raw }}</div>
          <div class="dim sm" style="margin-top: 8px">终稿（防污染后 · {{ asrRes.tier }} · {{ asrRes.ms }}ms）</div>
          <div class="po-text">{{ asrRes.text }}</div>
          <div v-if="asrRes.changes.length" class="po-changes">
            <span v-for="(c, i) in asrRes.changes" :key="i" class="chg" :class="c.layer">
              <s>{{ c.from }}</s> → <b>{{ c.to }}</b>
            </span>
          </div>
        </div>
      </section>

      <!-- 个人词表 · 热词 -->
      <section class="block" v-if="lex">
        <div class="b-head">
          <h2>个人词表 · 热词</h2>
          <span class="b-desc">高频专名，识别时偏置 + 拼音模糊回填的目标（共 {{ lex.hotwords.length }} 个）</span>
        </div>
        <div class="row">
          <input v-model="newHotword" class="in" placeholder="加热词，如 Polaris / 感官坞" @keydown.enter="addHotword" />
          <button class="btn sm" @click="addHotword">加入</button>
        </div>
        <div class="chips">
          <span v-for="w in lex.hotwords" :key="w" class="chip">
            {{ w }}<button class="x" @click="removeHotword(w)">×</button>
          </span>
        </div>
      </section>

      <!-- 个人词表 · 纠错映射 -->
      <section class="block" v-if="lex">
        <div class="b-head">
          <h2>同音/歧义纠错</h2>
          <span class="b-desc">错词 → 规范词（精确替换，跨脚本也能纠：扣带式 → codex）</span>
        </div>
        <div class="row">
          <input v-model="newWrong" class="in sm" placeholder="错词（扣带式）" />
          <span class="arr">→</span>
          <input v-model="newRight" class="in sm" placeholder="规范词（codex）" @keydown.enter="addCorrection" />
          <button class="btn sm" @click="addCorrection">加入</button>
        </div>
        <div class="corr-list">
          <div v-for="c in correctionList" :key="c.wrong" class="corr-row">
            <span class="cw"><s>{{ c.wrong }}</s> → <b>{{ c.right }}</b></span>
            <button class="x" @click="removeCorrection(c.wrong)">删除</button>
          </div>
          <div v-if="!correctionList.length" class="dim sm">暂无纠错映射</div>
        </div>
      </section>

      <!-- 词表自学 -->
      <section class="block">
        <div class="b-head">
          <h2>从历史学词</h2>
          <span class="b-desc">贴一段你常说/常写的文本，自动抽高频技术专名并入热词（自动化将搭回声层「每日做梦」周期跑）</span>
        </div>
        <textarea v-model="learnIn" class="ta" rows="3" placeholder="贴对话/文档片段…"></textarea>
        <div class="row">
          <button class="btn primary sm" :disabled="learnBusy || !learnIn.trim()" @click="runLearn">
            {{ learnBusy ? "学习中…" : "抽词并入库" }}
          </button>
        </div>
        <div v-if="mined.length" class="chips">
          <span v-for="m in mined" :key="m.term" class="chip mined">{{ m.term }}<em>{{ m.count }}</em></span>
        </div>
      </section>
    </template>
  </div>
</template>

<style scoped>
.voice {
  flex: 1;
  overflow-y: auto;
  padding: 40px 56px 80px;
  max-width: 980px;
  margin: 0 auto;
  width: 100%;
}
.head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  border-bottom: 1px solid var(--hairline);
  padding-bottom: 18px;
  margin-bottom: 22px;
}
.head h1 {
  font-family: var(--serif);
  font-size: 22px;
  font-weight: 500;
  letter-spacing: 2px;
  margin: 0 0 8px;
  color: var(--ink);
}
.head .sub {
  font-size: 12.5px;
  color: var(--muted);
  margin: 0;
  line-height: 1.8;
  max-width: 720px;
}
.dim {
  color: var(--dim);
}
.sm {
  font-size: 11.5px;
}
.err-line {
  color: #c0392b;
  font-size: 12.5px;
  margin: 8px 0;
}
.block {
  background: var(--panel);
  border: 1px solid var(--hairline);
  border-radius: 4px;
  padding: 16px 18px;
  margin-bottom: 16px;
  box-shadow: var(--shadow-sm);
}
.b-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 12px;
  flex-wrap: wrap;
}
.b-head h2 {
  font-family: var(--serif);
  font-size: 15px;
  font-weight: 600;
  letter-spacing: 1px;
  color: var(--ink);
  margin: 0;
}
.b-desc {
  font-size: 11.5px;
  color: var(--dim);
}
.row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 14px;
  margin: 8px 0;
}
.seg {
  display: inline-flex;
  border: 1px solid var(--border);
  border-radius: 4px;
  overflow: hidden;
}
.seg button {
  padding: 6px 14px;
  font-size: 12.5px;
  background: transparent;
  border: none;
  border-right: 1px solid var(--border);
  color: var(--text-2);
  cursor: pointer;
}
.seg button:last-child {
  border-right: none;
}
.seg button.on {
  background: var(--primary);
  color: #fff;
}
.fl {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
  color: var(--text-2);
}
.fl.fill {
  display: flex;
  width: 100%;
}
.fl.fill .in {
  flex: 1;
  min-width: 0;
}
.sw {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
  color: var(--text-2);
  cursor: pointer;
}
select,
.in,
.ta {
  background: var(--panel);
  color: var(--text);
  border: 1px solid var(--border);
  border-radius: 3px;
  padding: 6px 9px;
  font-size: 12.5px;
  font-family: inherit;
}
select.wide {
  min-width: 360px;
}
.in {
  min-width: 200px;
}
.in.sm {
  min-width: 130px;
}
.ta {
  width: 100%;
  resize: vertical;
  line-height: 1.6;
}
select:focus,
.in:focus,
.ta:focus {
  outline: none;
  border-color: var(--primary);
}
.arr {
  color: var(--dim);
}
.btn {
  padding: 7px 14px;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 3px;
  color: var(--text-2);
  font-size: 12.5px;
  cursor: pointer;
  white-space: nowrap;
}
.btn:hover:not(:disabled) {
  border-color: var(--ink);
  color: var(--ink);
}
.btn:disabled {
  opacity: 0.5;
  cursor: default;
}
.btn.sm {
  padding: 5px 12px;
  font-size: 12px;
}
.btn.primary {
  border-color: var(--primary);
  color: var(--primary);
}
.btn.primary.on {
  background: var(--primary);
  color: #fff;
}
.tierhint code {
  font-family: var(--mono);
  font-size: 11px;
  background: var(--bg-soft);
  padding: 1px 5px;
  border-radius: 3px;
}
.tierhint {
  font-size: 12px;
  color: var(--text-2);
  background: var(--bg-soft);
  border-radius: 3px;
  padding: 8px 12px;
  line-height: 1.7;
  margin-top: 6px;
}
.tierhint .warn {
  color: #b8860b;
}
.probe-out {
  margin-top: 10px;
  border-top: 1px solid var(--hairline);
  padding-top: 10px;
}
.po-text {
  font-size: 13.5px;
  color: var(--ink);
  line-height: 1.8;
  background: var(--bg-soft);
  border-radius: 3px;
  padding: 8px 12px;
}
.po-changes {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 8px;
}
.chg {
  font-size: 11.5px;
  border: 1px solid var(--hairline);
  border-radius: 3px;
  padding: 2px 8px;
  color: var(--text-2);
}
.chg s {
  color: #c0392b;
}
.chg b {
  color: #2e8b57;
}
.chg em {
  font-style: normal;
  color: var(--dim);
  margin-left: 6px;
  font-size: 10px;
}
.chg.pinyin {
  border-color: #6c92c455;
}
.chg.exact {
  border-color: #d4b06a55;
}
.chips {
  display: flex;
  flex-wrap: wrap;
  gap: 7px;
  margin-top: 8px;
}
.chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 12px;
  color: var(--text-2);
  border: 1px solid var(--border);
  border-radius: 14px;
  padding: 3px 6px 3px 11px;
}
.chip.mined {
  border-style: dashed;
}
.chip em {
  font-style: normal;
  color: var(--dim);
  font-size: 10px;
  margin-left: 2px;
}
.chip .x {
  background: none;
  border: none;
  color: var(--dim);
  cursor: pointer;
  font-size: 14px;
  line-height: 1;
  padding: 0 2px;
}
.chip .x:hover {
  color: #c0392b;
}
.corr-list {
  margin-top: 8px;
}
.corr-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  border-bottom: 1px dashed var(--hairline);
  padding: 6px 0;
  font-size: 12.5px;
}
.cw s {
  color: #c0392b;
}
.cw b {
  color: #2e8b57;
}
.corr-row .x {
  background: none;
  border: none;
  color: #c0392b;
  font-size: 11.5px;
  cursor: pointer;
  opacity: 0.7;
}
.corr-row .x:hover {
  opacity: 1;
}
</style>
