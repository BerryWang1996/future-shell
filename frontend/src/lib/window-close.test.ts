import { describe, it, expect, vi } from "vitest";
import fs from "node:fs";
import path from "node:path";
import { createCloseGate, decideClose, isLiveSession, type CloseTabStatus } from "./window-close";

/**
 * 窗口关闭闸门（审计：事件契约门禁扫出的第三处缺陷——`tauri://close-requested` 零处理者）。
 *
 * 这个两态机的两条边互为反面，任一条写错都不出声音：
 * - 少了 `preventDefault()`：× 照常关窗，确认框来不及显示，在途传输静默中断 —— 即修复前的原状；
 * - 少了放行位：确认之后的 `close()` 又被自己拦下，弹第二个确认框 —— 窗口永远关不掉。
 * 所以两条边必须各有一条会因对方缺失而转红的用例，且必须有一条用例真的走完
 * 「× → 拦 → 确认 → 关掉」的整条路径（只测单边会让「拦住了但再也关不掉」全绿通过）。
 */
describe("窗口关闭闸门", () => {
  function harness() {
    const close = vi.fn(async () => {});
    const prevent = vi.fn();
    return { close, prevent };
  }

  it("首次关闭请求：拦下并转交确认流程，不关窗", async () => {
    const { close, prevent } = harness();
    const ask = vi.fn();
    const gate = createCloseGate(close, ask);

    await expect(gate.onCloseRequested(prevent)).resolves.toBe(false);
    expect(prevent).toHaveBeenCalledTimes(1);
    expect(ask).toHaveBeenCalledTimes(1);
    expect(close, "拦截阶段绝不能关窗——那正是 × 绕过门控的原状").not.toHaveBeenCalled();
  });

  it("确认后关得掉：closeConfirmed 之后的关闭请求一律放行", async () => {
    const { close, prevent } = harness();
    let asked = 0;
    // ask 模拟真实接线：弹框 → 用户点确认 → onConfirm 调 closeConfirmed()。
    // `asked > 1` 那道闸不是装饰：把重入放在 ask 里递归写，放行位一旦失效就是**无界递归**，
    // vitest 的 worker 会直接崩掉——而崩掉的 worker 在 JSON 报告里是 `success:true`、
    // numFailedTests=0，只有 numPassedTests 悄悄少 4。变异校验实测撞上过一次：
    // 一条本该转红的变异被判成"幸存"。守卫必须以**断言**转红，不能以拖垮 runner 表达失败。
    const gate = createCloseGate(close, async () => {
      if (++asked > 1) return;
      await gate.closeConfirmed();
    });

    await gate.onCloseRequested(prevent); // ① × 触发：拦下 → 确认 → 关窗
    expect(close).toHaveBeenCalledTimes(1);

    // ② closeConfirmed 里的 close() 在真机上会再次触发 close-requested，此处显式复现这一次重入。
    await expect(
      gate.onCloseRequested(prevent),
      "确认后仍被拦 = 点了确认窗口不关，且会再弹一个确认框",
    ).resolves.toBe(true);
    expect(prevent, "重入那一次不得再拦").toHaveBeenCalledTimes(1);
    expect(asked, "重入不得再弹一次确认框").toBe(1);
  });

  it("放行位只由 closeConfirmed 抬起：ask 抛异常也不会漏关", async () => {
    const { close, prevent } = harness();
    const gate = createCloseGate(close, () => {
      throw new Error("确认框渲染失败");
    });
    // 先 preventDefault 再 await ask()，故异常穿出去时窗口已经拦住了；
    // 反过来写（先 await 后 prevent）会让一次渲染失败变成「窗口直接销毁」。
    await expect(gate.onCloseRequested(prevent)).rejects.toThrow("确认框渲染失败");
    expect(prevent).toHaveBeenCalledTimes(1);
    expect(close).not.toHaveBeenCalled();
  });

  /**
   * 结构钉（App.svelte 无组件测试，装配层只能这么核）：闸门必须真的被订阅到
   * `onCloseRequested` 上，且窗口关闭只许经闸门出口走。
   * 上面三条用例证的是闸门本身的行为；接不上去的话它们全绿而缺陷原封不动。
   */
  it("App.svelte 把闸门接到 onCloseRequested，且不再有绕过闸门的 close()", () => {
    const app = fs.readFileSync(path.resolve(process.cwd(), "src/App.svelte"), "utf8");
    expect(app).toContain("createCloseGate(");
    expect(app, "×/Alt+F4 的关闭请求没有订阅者 = 门控只覆盖菜单退出").toMatch(
      /getCurrentWindow\(\)\.onCloseRequested\(/,
    );
    expect(app).toMatch(/closeGate\.onCloseRequested\(\(\) => event\.preventDefault\(\)\)/);

    // 光有函数体不算接线。`bridgeWindowClose()` 若定义了却没人调用，上面几条断言（它们只看
    // 文本存在与否）会全绿，而 ×/Alt+F4 照样绕过门控——缺陷原封不动地回来了。所以必须钉
    // **调用点**：它得排在 onMount 那串订阅里，和另外两个 bridge 挨着。
    // 2026-09-02（路线图 4c A3）起 onMount 的每一步经 `boot(name, fn)` 隔离，调用点形如
    // `await boot("bridgeWindowClose", bridgeWindowClose);`——钉子同时认这一形态；裸 `bridgeWindowClose();` 亦可。
    expect(app, "bridgeWindowClose 只定义未调用 = 等于没订阅 close-requested").toMatch(
      /(?:bridgeSessionEvents\(\);|boot\("bridgeSessionEvents", bridgeSessionEvents\);)[\s\S]{0,400}?\n\s*(?:bridgeWindowClose\(\);|await boot\("bridgeWindowClose", bridgeWindowClose\);)/,
    );

    // 关窗只许经 closeGate.closeConfirmed()：任何一处直接 `getCurrentWindow().close()`
    // 都是一条绕过确认的暗道（修复前 requestCloseWindow 里就有两处）。
    // 例外是闸门自己的出口——它作为 close 回调传进 createCloseGate。
    const directCloses = [...app.matchAll(/getCurrentWindow\(\)\.close\(\)/g)].length;
    expect(directCloses, "getCurrentWindow().close() 只应出现在传给 createCloseGate 的那个回调里").toBe(1);
    expect(app).toMatch(/createCloseGate\(\s*\(\) => getCurrentWindow\(\)\.close\(\)/);
    expect((app.match(/closeGate\.closeConfirmed\(\)/g) ?? []).length, "两条确认出口（无会话直关 / 确认框 onConfirm）都要走闸门").toBe(2);
  });
});

/**
 * 关闭确认的判据（M1 出口「关闭确认」条）。
 *
 * 这条判据在两个 scope 里各写各的，于是各错一半：标签级只数传输（活连接被静默掐掉），
 * 窗口级数 tabs.length（全是残标签也弹框）。收敛到 decideClose 之后，下面的用例
 * 必须同时钉住**两个方向**——只测「该弹的弹了」，把判据写成恒真也全绿。
 */
describe("关闭确认判据 decideClose", () => {
  const tab = (id: string, status: CloseTabStatus) => ({ id, status });
  const none = { queued: 0, running: 0 };

  it("活动会话（connected/connecting）无传输也必须弹框——修复前 Ctrl+W 静默断开", () => {
    for (const st of ["connected", "connecting"] as CloseTabStatus[]) {
      const d = decideClose([tab("s1", st)], none);
      expect(d.prompt, `status=${st} 应弹框`).toBe(true);
      expect(d.sessions).toEqual(["s1"]);
    }
  });

  it("全是断开/错误态的残标签且无传输 → 直接关，不弹框（出口「全空闲直接关闭」）", () => {
    const d = decideClose([tab("a", "disconnected"), tab("b", "error")], none);
    expect(d.prompt, "残标签不该触发确认框——修复前窗口级用 tabs.length，这里会是 true").toBe(false);
    expect(d.sessions).toEqual([]);
  });

  it("零标签零传输 → 不弹框", () => {
    expect(decideClose([], none).prompt).toBe(false);
  });

  it("残标签 + 在途传输 → 仍要弹（判据是「或」），但会话数为 0 不虚报", () => {
    const d = decideClose([tab("a", "disconnected")], { queued: 2, running: 1 });
    expect(d.prompt).toBe(true);
    expect(d.sessions, "断开态标签不该被算成「将断开的会话」").toEqual([]);
    expect(d.queued + d.running).toBe(3);
  });

  it("混合标签只列活动的那些（弹框的「会话数」不含残骸）", () => {
    const d = decideClose(
      [tab("dead", "disconnected"), tab("live", "connected"), tab("dialing", "connecting"), tab("bad", "error")],
      none,
    );
    expect(d.sessions).toEqual(["live", "dialing"]);
  });

  it("四态里恰有两态算「活动」——判据写成恒真/恒假都会红", () => {
    const live = (["connecting", "connected", "disconnected", "error"] as CloseTabStatus[]).filter(isLiveSession);
    expect(live).toEqual(["connecting", "connected"]);
  });

  it("传输计数取非负：负数（计数器竞态）不得把 prompt 拉回 false", () => {
    const d = decideClose([], { queued: -5, running: 1 });
    expect(d.queued).toBe(0);
    expect(d.running).toBe(1);
    expect(d.prompt, "running=1 就该弹，不该被 queued 的负数抵消掉").toBe(true);
  });
});
