/**
 * rdp-frames.ts — RDP 帧的 raw 直送通道（4d：帧管道 `tauri::ipc::Channel` raw）。
 *
 * # 生命周期问题决定了一切形状
 *
 * Channel 必须在 `rdp_connect` **之前**创建（它是连接命令的参数），而画布组件
 * `RdpPane` 要到连接成功、标签转正**之后**才挂载。中间那段「通道已建、画布未到」
 * 的帧不能丢——所以通道本体放这张模块级表里，画布来认领；画布卸载（切标签）只解绑
 * 回调不拆通道，通道跟着会话活到关闭。
 *
 * # 帧格式（与 rdp.rs `frame_wire` 对偶）
 *
 * 裸字节：`x/y/w/h` 各 4 字节**小端**，随后 RGBA。两端都显式按小端读写
 * （`DataView.getUint32(off, true)` / `to_le_bytes`），字节序是契约的一部分。
 *
 * # 回执（ack）不变
 *
 * 换的只是「帧怎么到前端」，背压协议一行没动：画布每画完一帧照旧 `rdp_frame_ack`。
 * helper 侧的配额计数（在途上限）漏 ack 就会永久冻死——这在 raw 路径上同样成立。
 */
import { Channel } from "@tauri-apps/api/core";

/** 画一帧 raw 载荷（ArrayBuffer）。抛错 = 这块像素没画上，调用方负责索要整屏。 */
export type RawFramePainter = (buf: ArrayBuffer) => void;

interface Sink {
  channel: Channel<ArrayBuffer>;
  /** 当前认领者（RdpPane 挂载时装上、卸载时置 null）。帧在无人认领时**缓存最后一帧**，
   * 画布来了立刻补画——切标签回来不该是一块黑屏等下一次脏矩形。 */
  painter: RawFramePainter | null;
  lastFrame: ArrayBuffer | null;
}

const sinks = new Map<string, Sink>();

/** 建一个帧通道（调用 `rdp_connect` 之前）。同一会话重复调用**替换**旧通道——
 * 上一条通道闭包里的回调状态已经没有意义。 */
export function createFrameChannel(sessionId: string): Channel<ArrayBuffer> {
  const channel = new Channel<ArrayBuffer>();
  const sink: Sink = { channel, painter: null, lastFrame: null };
  channel.onmessage = (buf) => {
    if (!(buf instanceof ArrayBuffer)) return; // 防御：raw 之外的消息形状不认识，丢弃
    if (sink.painter) {
      sink.painter(buf);
    } else {
      sink.lastFrame = buf;
    }
  };
  sinks.set(sessionId, sink);
  return channel;
}

/** 占位 id → 真 session_id：`rdp_connect` 返回后立刻调用（通道对象不变，只改表键）。 */
export function rebindFrameChannel(pendingId: string, sessionId: string): void {
  const sink = sinks.get(pendingId);
  if (!sink || sessionId === pendingId) return;
  sinks.delete(pendingId);
  sinks.set(sessionId, sink);
}

/** 画布认领这个会话的帧。返回它错过的一帧（若有）——调用方应当先补画再等新帧。 */
export function claimFramePainter(sessionId: string, painter: RawFramePainter): ArrayBuffer | null {
  const sink = sinks.get(sessionId);
  if (!sink) return null;
  sink.painter = painter;
  const missed = sink.lastFrame;
  sink.lastFrame = null;
  return missed;
}

/** 画布卸载：只解绑回调，通道留着（会话还在，切回来还要用）。 */
export function releaseFramePainter(sessionId: string): void {
  const sink = sinks.get(sessionId);
  if (sink) sink.painter = null;
}

/** 会话关闭：通道与缓存帧一并丢弃。缓存不丢的话，下一个同 id 会话（重连后端会给新 id，
 * 但防御性地）会先收到一块旧画面的残影。 */
export function dropFrameChannel(sessionId: string): void {
  sinks.delete(sessionId);
}

/** 这个会话有没有已建的帧通道（RdpPane 据此决定听 raw 还是听事件）。 */
export function hasFrameChannel(sessionId: string): boolean {
  return sinks.has(sessionId);
}

/** 仅测试用。 */
export function resetFrameSinksForTest(): void {
  sinks.clear();
}

/** 解一条 raw 载荷。独立出来是因为它与 Rust 侧 `frame_wire` 的 16 字节小端头
 * 逐字对偶——两边的对齐靠各自的单测钉住同一组字节。 */
export function decodeFrame(buf: ArrayBuffer): { x: number; y: number; w: number; h: number; rgba: Uint8Array } {
  if (buf.byteLength < 16) throw new Error(`帧载荷过短（${buf.byteLength} 字节，头就要 16）`);
  const dv = new DataView(buf);
  const x = dv.getUint32(0, true);
  const y = dv.getUint32(4, true);
  const w = dv.getUint32(8, true);
  const h = dv.getUint32(12, true);
  const rgba = new Uint8Array(buf, 16);
  if (w === 0 || h === 0) throw new Error("帧尺寸为零");
  if (rgba.byteLength < w * h * 4) {
    throw new Error(`像素不足：头说 ${w}×${h}（要 ${w * h * 4} 字节），载荷只有 ${rgba.byteLength}`);
  }
  return { x, y, w, h, rgba };
}
