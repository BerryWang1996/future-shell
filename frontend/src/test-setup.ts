// S266（测试环境自洽性修复，不涉产物）：jsdom 环境下 `TextEncoder` 与 `Uint8Array` 分属两个 realm。
// vitest 的 jsdom 环境把 window 的内建对象装入 globalThis，故 `new Uint8Array()`（如 term.ts
// `decodeB64`）取的是 **jsdom vm 上下文**的构造器；而 jsdom 的 `window.TextEncoder` 直接转发
// Node 的 `util.TextEncoder`，其 `encode()` 返回 **Node realm** 的 Uint8Array。二者字节完全相同
// 但 `constructor` 不是同一个对象，vitest `toEqual` 对 TypedArray 比对构造器身份 ⇒ 报
// 「expected Uint8Array[…] to deeply equal Uint8Array[…] / Compared values have no visual difference」。
//
// 取舍留字：修的是环境而非断言。判据是「谁不自洽」——jsdom 自己的 TextEncoder 吐出非自己 realm 的
// TypedArray 属环境缺陷，与被测代码无关；若改测试为 `Array.from(a)).toEqual(Array.from(b))` 则连
// 「返回值确为 Uint8Array」这一条也一并放弃，且此坑会在日后每个用到 TextEncoder 的用例上重犯。
// 另一流行解法是把 `globalThis.Uint8Array` 换成 Node realm 的，但那会波及 Blob/crypto 等以
// jsdom realm TypedArray 做 instanceof 判定的 DOM API，面比这里宽得多，故不取。
const ForeignRealmTextEncoder = globalThis.TextEncoder;

class LocalRealmTextEncoder extends ForeignRealmTextEncoder {
  override encode(input?: string): Uint8Array<ArrayBuffer> {
    // Uint8Array.from 走 globalThis 上的构造器 ⇒ 与 decodeB64 等测试内代码同 realm。
    return Uint8Array.from(super.encode(input));
  }
}

globalThis.TextEncoder = LocalRealmTextEncoder;

// M7.3：jsdom 没有 ResizeObserver，而虚拟窗口画布靠它盯自己的尺寸（拿 window.innerWidth
// 去夹紧会把窗口推到状态栏底下——出口标准要禁掉的正是「拖丢了找不回来」）。
//
// 这里补一个**受控的**桩而不是空壳：`ResizeObserverStub.instances` 暴露出去，
// 需要模拟「画布尺寸变了」的测试可以自己触发回调。空壳的话那条路径在测试里永远不执行，
// 而它恰恰是「主窗口缩小 → 窗口被挤出去」的唯一防线。
class ResizeObserverStub {
  static instances: ResizeObserverStub[] = [];
  constructor(readonly cb: ResizeObserverCallback) {
    ResizeObserverStub.instances.push(this);
  }
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {
    const i = ResizeObserverStub.instances.indexOf(this);
    if (i >= 0) ResizeObserverStub.instances.splice(i, 1);
  }
  /** 测试用：手动触发一次回调（参数不重要，画布是自己去读 clientWidth 的）。 */
  fire(): void {
    this.cb([], this as unknown as ResizeObserver);
  }
}
if (!("ResizeObserver" in globalThis)) {
  (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = ResizeObserverStub;
}
