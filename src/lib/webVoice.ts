// 浏览器端语音采集 —— 远程/Docker 部署专用。
//
// 桌面版靠后端 cpal 在「跑后端的机器」上录音(voice_live.rs);但远程部署里后端是 NAS,
// 麦克风在用户面前的浏览器,服务端没有麦克风。所以采集必须搬到客户端:
//   getUserMedia → 录 Float32 → 重采样 16k 单声道 → 编 16-bit PCM WAV → 交给调用方上传。
// 服务端再用既有 voice_transcribe_file(sherpa 读 WAV)识别 + 防污染,与桌面同一条管线。
//
// ⚠ getUserMedia 需「安全上下文」(HTTPS 或 localhost),纯 http:// 的局域网/Tailscale IP
//   会被浏览器直接拒。远程访问请用 `tailscale serve` 套 HTTPS,或 localhost 端口转发。

/** 线性插值重采样到 16kHz(镜像 voice_live.rs 的 resample_to_16k,保证两端口径一致)。 */
function resampleTo16k(samples: Float32Array, inRate: number): Float32Array {
  if (inRate === 16000 || samples.length === 0) return samples;
  const ratio = 16000 / inRate;
  const outLen = Math.floor(samples.length * ratio);
  const out = new Float32Array(outLen);
  const last = samples.length - 1;
  for (let i = 0; i < outLen; i++) {
    const src = i / ratio;
    const i0 = Math.floor(src);
    const i1 = Math.min(i0 + 1, last);
    const frac = src - i0;
    out[i] = samples[i0] * (1 - frac) + samples[i1] * frac;
  }
  return out;
}

/** Float32 [-1,1] 单声道 → 16-bit PCM WAV(44 字节头 + 数据),返回 Blob。 */
function encodeWav16(samples: Float32Array, sampleRate: number): Blob {
  const dataLen = samples.length * 2;
  const buf = new ArrayBuffer(44 + dataLen);
  const dv = new DataView(buf);
  const wstr = (off: number, s: string) => {
    for (let i = 0; i < s.length; i++) dv.setUint8(off + i, s.charCodeAt(i));
  };
  wstr(0, "RIFF");
  dv.setUint32(4, 36 + dataLen, true);
  wstr(8, "WAVE");
  wstr(12, "fmt ");
  dv.setUint32(16, 16, true); // fmt chunk size
  dv.setUint16(20, 1, true); // PCM
  dv.setUint16(22, 1, true); // 单声道
  dv.setUint32(24, sampleRate, true);
  dv.setUint32(28, sampleRate * 2, true); // byte rate
  dv.setUint16(32, 2, true); // block align
  dv.setUint16(34, 16, true); // bits per sample
  wstr(36, "data");
  dv.setUint32(40, dataLen, true);
  let off = 44;
  for (let i = 0; i < samples.length; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    dv.setInt16(off, s < 0 ? s * 0x8000 : s * 0x7fff, true);
    off += 2;
  }
  return new Blob([buf], { type: "audio/wav" });
}

/**
 * 浏览器麦克风录音器:start() 开录,stop() 停录并产出 16k 单声道 WAV File。
 * 用 ScriptProcessor 收 Float32(AudioWorklet 要独立 worklet 文件,跨 Vite 打包更麻烦,
 * 而批处理模式对延迟不敏感,ScriptProcessor 足够)。
 */
export class WebVoiceRecorder {
  private ctx: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private node: ScriptProcessorNode | null = null;
  private src: MediaStreamAudioSourceNode | null = null;
  private chunks: Float32Array[] = [];
  private inRate = 16000;

  async start(): Promise<void> {
    if (!window.isSecureContext) {
      throw new Error("浏览器禁止非安全连接访问麦克风;请用 HTTPS(tailscale serve)或 localhost 访问");
    }
    if (!navigator.mediaDevices?.getUserMedia) {
      throw new Error("当前浏览器不支持麦克风采集(getUserMedia)");
    }
    this.stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
    });
    const Ctor = window.AudioContext || (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
    this.ctx = new Ctor();
    this.inRate = this.ctx.sampleRate;
    this.src = this.ctx.createMediaStreamSource(this.stream);
    const proc = this.ctx.createScriptProcessor(4096, 1, 1);
    this.chunks = [];
    proc.onaudioprocess = (e) => {
      // 拷贝一份:inputBuffer 会被复用,直接存引用会被后续帧覆盖。
      this.chunks.push(new Float32Array(e.inputBuffer.getChannelData(0)));
    };
    this.src.connect(proc);
    proc.connect(this.ctx.destination); // 不连目的地部分浏览器不触发回调
    this.node = proc;
  }

  /** 停止采集并释放设备 → 16k 单声道 WAV File;录音过短(<0.2s)返回 null(误触)。 */
  async stop(): Promise<File | null> {
    const { node, src, ctx, stream, inRate } = this;
    const chunks = this.chunks;
    this.node = null;
    this.src = null;
    this.ctx = null;
    this.stream = null;
    this.chunks = [];
    try {
      node?.disconnect();
      src?.disconnect();
    } catch {
      /* ignore */
    }
    stream?.getTracks().forEach((t) => t.stop());
    try {
      await ctx?.close();
    } catch {
      /* ignore */
    }

    const total = chunks.reduce((n, c) => n + c.length, 0);
    if (total < inRate * 0.2) return null; // <0.2s:误触,不识别
    const merged = new Float32Array(total);
    let off = 0;
    for (const c of chunks) {
      merged.set(c, off);
      off += c.length;
    }
    const out = resampleTo16k(merged, inRate);
    return new File([encodeWav16(out, 16000)], "voice.wav", { type: "audio/wav" });
  }

  /** 取消:停采集、丢缓冲、不产出。 */
  cancel(): void {
    void this.stop().catch(() => {});
    this.chunks = [];
  }
}
