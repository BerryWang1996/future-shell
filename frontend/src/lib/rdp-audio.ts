/**
 * RDP 音频播放（阶段 3，RDPSND 的前端半边）。
 *
 * # 为什么播放在前端
 *
 * Rust 侧播放要引 cpal——一棵新依赖树、Linux 构建要 ALSA 头、还多一份出站面
 * 审计。webview 自带 Web Audio API，且它的音量归系统混音器管（与用户在别处的
 * 音量控制是同一套）。
 *
 * # 排程：为什么不能收到就 start()
 *
 * 每段 PCM 立即 `start()` 会让两段之间出现调度抖动造成的静音缝——听感是
 * 持续的「咔哒」。正确做法是维护一条**播放时间线**：每段排在前一段结束的
 * 那一刻。这就是 `nextStartTime` 的全部作用。
 *
 * # 积压要丢，而且丢的是**旧的**
 *
 * 音频与画面的取舍相反：画面可以合并（后写覆盖前写，见 helper 的 fb.rs），
 * 音频不能叠加（两段声音相加是噪音）。而积压的音频也不能全放——人耳听得出
 * 200ms 的不同步。故超过 [`MAX_BUFFER_SECONDS`] 的积压直接把时间线**拉回
 * 当下**：丢掉的是排在队尾还没播的那些，代价是一次轻微的跳跃，
 * 换来音画重新对齐。
 */

/** 播放缓冲上限（秒）。超过即丢弃积压重新对齐——见模块头。 */
export const MAX_BUFFER_SECONDS = 0.35;

export interface PcmFormat {
  sampleRate: number;
  channels: number;
  bitsPerSample: number;
}

/**
 * 16 位小端交错 PCM → Web Audio 的分声道 Float32。
 *
 * 三件容易错的：
 * · **小端**（RDP 的 PCM 是小端，读成大端得到的是刺耳噪音）；
 * · **有符号**（i16 而不是 u16；读成无符号会把波形整体抬高一半量程 = 直流偏置 + 削顶）；
 * · 归一化除数是 32768（i16 的负半程幅度），不是 32767——用后者会让
 *   最小值 -32768 归一化到 -1.0000305，超出 Float32 音频的 [-1, 1] 约定。
 */
export function decodePcm16(
  bytes: Uint8Array,
  channels: number,
): Float32Array<ArrayBuffer>[] {
  const frameCount = Math.floor(bytes.length / 2 / channels);
  // 显式标注 ArrayBuffer（而不是让它推成 ArrayBufferLike）：
  // copyToChannel 拒收可能是 SharedArrayBuffer 的那个宽类型。
  const out: Float32Array<ArrayBuffer>[] = [];
  for (let c = 0; c < channels; c++) out.push(new Float32Array(frameCount));
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  for (let f = 0; f < frameCount; f++) {
    for (let c = 0; c < channels; c++) {
      const sample = view.getInt16((f * channels + c) * 2, /* littleEndian */ true);
      out[c][f] = sample / 32768;
    }
  }
  return out;
}

/**
 * 一条会话的音频播放器。
 *
 * 生命周期与 RDP 会话对齐：`close()` 之后本对象作废（下一次连接建新的）。
 */
export class RdpAudioPlayer {
  private ctx: AudioContext | null = null;
  private format: PcmFormat | null = null;
  /** 下一段该排在什么时刻（AudioContext 时间轴，秒）。 */
  private nextStartTime = 0;
  /** 因积压被丢弃的段数——诊断用（用户报「声音断断续续」时先看这个）。 */
  dropped = 0;

  /** 远端通告了新格式：重开上下文（采样率是 AudioContext 的构造参数，改不了）。 */
  setFormat(fmt: PcmFormat): void {
    if (
      this.format &&
      this.format.sampleRate === fmt.sampleRate &&
      this.format.channels === fmt.channels &&
      this.format.bitsPerSample === fmt.bitsPerSample
    ) {
      return; // 同一格式重复通告：什么都不做（重开设备会爆音）
    }
    this.close();
    this.format = fmt;
    this.ctx = new AudioContext({ sampleRate: fmt.sampleRate });
    this.nextStartTime = 0;
  }

  /** 播一段 PCM。格式未知（没收到 AudioFormat）时丢弃——猜格式播出来是噪音。 */
  push(bytes: Uint8Array): void {
    const ctx = this.ctx;
    const fmt = this.format;
    if (!ctx || !fmt) return;
    if (fmt.bitsPerSample !== 16) return; // 只支持 16 位（helper 只通告这一种）

    const planes = decodePcm16(bytes, fmt.channels);
    const frameCount = planes[0]?.length ?? 0;
    if (frameCount === 0) return;

    // 积压过深：把时间线拉回当下，丢掉还没播的那些（见模块头）
    const now = ctx.currentTime;
    if (this.nextStartTime > now + MAX_BUFFER_SECONDS) {
      this.dropped += 1;
      this.nextStartTime = now;
    }
    // 时间线落后于当下（首段，或上一段早已播完）：从当下开始
    if (this.nextStartTime < now) this.nextStartTime = now;

    const buf = ctx.createBuffer(fmt.channels, frameCount, fmt.sampleRate);
    for (let c = 0; c < fmt.channels; c++) buf.copyToChannel(planes[c], c);
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(ctx.destination);
    src.start(this.nextStartTime);
    this.nextStartTime += frameCount / fmt.sampleRate;
  }

  /** 关闭并释放设备。幂等。 */
  close(): void {
    if (this.ctx) {
      void this.ctx.close().catch(() => {});
      this.ctx = null;
    }
    this.format = null;
    this.nextStartTime = 0;
  }
}
