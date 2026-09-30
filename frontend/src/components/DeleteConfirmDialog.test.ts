import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import DeleteConfirmDialog from "./DeleteConfirmDialog.svelte";
// 取 App.svelte 源文本走 vite 的 ?raw（同 menus.test.ts / session-restore.test.ts 的既有口径：
// tsconfig 下 node:fs 与 vitest 模块运行器的 import.meta.url 都不可靠）。
import APP_SOURCE from "../App.svelte?raw";

/**
 * 删除确认框（App.svelte 侧栏右键「删除…」的唯一确认面）。
 *
 * 这些用例守的不是「弹窗弹出来了」，而是**失败方向**：删除连接不可撤销，误删的代价是
 * 用户手工重建主机/端口/认证配置（凭据引用还可能一并失联）。所以这里每一条都在钉
 * 「不小心时会发生什么」，而不是「操作正确时会发生什么」——
 * 前者才是这个组件存在的理由，也正是它被原生 `confirm()` 顶替期间丢掉的东西
 * （原生 confirm 默认按钮是「确定」，回车即删，方向恰好相反）。
 */
describe("DeleteConfirmDialog（破坏性操作确认，失败方向）", () => {
  const props = (over: Record<string, unknown> = {}) => ({
    open: true,
    name: "prod-db",
    onConfirm: vi.fn(),
    onCancel: vi.fn(),
    ...over,
  });

  it("默认焦点在「取消」而非「删除」：惯性回车不会删掉任何东西", async () => {
    const p = props();
    render(DeleteConfirmDialog, { props: p });
    const cancel = await screen.findByTestId("delete-cancel");
    // 焦点陷阱在 $effect 里装，等一拍微任务让它跑完
    await Promise.resolve();
    expect(document.activeElement).toBe(cancel);
    expect(p.onConfirm).not.toHaveBeenCalled();
  });

  it("Esc 走取消而不是确认（撤退键必须是安全键）", async () => {
    const p = props();
    render(DeleteConfirmDialog, { props: p });
    await fireEvent.keyDown(await screen.findByTestId("delete-confirm-dialog"), { key: "Escape" });
    expect(p.onCancel).toHaveBeenCalled();
    expect(p.onConfirm).not.toHaveBeenCalled();
  });

  it("点遮罩关闭 = 取消：点空白处不构成同意", async () => {
    const p = props();
    const { container } = render(DeleteConfirmDialog, { props: p });
    await fireEvent.click(container.querySelector(".overlay")!);
    expect(p.onCancel).toHaveBeenCalled();
    expect(p.onConfirm).not.toHaveBeenCalled();
  });

  it("点对话框本体不冒泡成取消（stopPropagation 生效，否则框内一切点击都在关窗）", async () => {
    const p = props();
    render(DeleteConfirmDialog, { props: p });
    await fireEvent.click(await screen.findByTestId("delete-msg"));
    expect(p.onCancel).not.toHaveBeenCalled();
    expect(p.onConfirm).not.toHaveBeenCalled();
  });

  it("只有按「删除」才 onConfirm，且恰好一次", async () => {
    const p = props();
    render(DeleteConfirmDialog, { props: p });
    await fireEvent.click(await screen.findByTestId("delete-ok"));
    expect(p.onConfirm).toHaveBeenCalledTimes(1);
    expect(p.onCancel).not.toHaveBeenCalled();
  });

  it("文案说的是实际会被删掉的东西：默认名词「连接」+ 目标名 + 不可撤销", async () => {
    render(DeleteConfirmDialog, { props: props() });
    const msg = await screen.findByTestId("delete-msg");
    // 原文案硬写「会话」，删的却是连接档案。文案与后果不符的确认框比没有确认框更坏：
    // 用户确认的是自己以为的那件事，删掉的是另一件，而且他不会去核对。
    expect(msg.textContent).toContain("连接");
    expect(msg.textContent).not.toContain("会话");
    expect(msg.textContent).toContain("prod-db");
    expect(msg.textContent).toContain("不可撤销");
  });

  it("what 可覆盖，供将来别的删除路径复用而不必改组件", async () => {
    render(DeleteConfirmDialog, { props: props({ what: "分组" }) });
    expect((await screen.findByTestId("delete-msg")).textContent).toContain("分组");
  });

  it("open=false 时整框不挂载（不是仅隐藏：隐藏着的确认框仍会被 testid/焦点找到）", () => {
    render(DeleteConfirmDialog, { props: props({ open: false }) });
    expect(screen.queryByTestId("delete-confirm-dialog")).toBeNull();
  });
});

/**
 * 上面几条只证明这个组件本身对；组件对而没人挂载，用户看到的还是原生 confirm。
 * 缺陷本体正是「组件写好了、全仓零引用」，所以必须另钉接线。
 * App.svelte 无法在组件层跑起来（onMount 串着 vault_status、事件订阅等一串副作用），
 * 故退一步做结构性钉——挡不住语义走样，但挡得住接线被回退或顺手删掉。
 */
describe("App.svelte 删除接线（结构性钉，口径同 session-restore.test.ts）", () => {
  it("侧栏删除走 DeleteConfirmDialog，且不再有原生 confirm()", () => {
    expect(APP_SOURCE).toContain('import DeleteConfirmDialog from "./components/DeleteConfirmDialog.svelte"');
    expect(APP_SOURCE).toMatch(/<DeleteConfirmDialog[\s\S]{0,200}?open=\{!!deleteConfirmState\}/);
    // onDelete 必须只登记确认态，删除动作留在 onConfirm 闭包里——
    // 若哪天有人把 profile_delete 挪回 onDelete，确认框就成了摆设（点开就已经删完了）。
    expect(APP_SOURCE).toMatch(/onDelete=\{\(p\) =>[\s\S]{0,120}?deleteConfirmState = \{/);
    expect(APP_SOURCE).toMatch(/onConfirm: async \(\) => \{[\s\S]{0,200}?invoke\("profile_delete"/);
    // 恰好一次：上一条只证明「onConfirm 里有一处删除」，挡不住在 onDelete 里**另加**一处——
    // 那样确认框照弹、档案却已经删了，是这条路径最坏的失败形态（用户点「取消」也没用）。
    expect(APP_SOURCE.match(/invoke\("profile_delete"/g)).toHaveLength(1);
    // 原生 confirm 的默认按钮是「确定」，回车即删，与本组件的失败方向相反；
    // 代码里不得再出现（alert 不在此列：About 框非破坏性，另有既有注记）。
    //
    // 扫的是**剥掉注释后**的源码：解释这次修复的注释本身就写着「原生 confirm()」，
    // 直接扫全文会被自己的注释绊住，然后人只会去改注释措辞——守卫就此变成一句空话。
    const code = APP_SOURCE
      .replace(/<!--[\s\S]*?-->/g, "")
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/(^|[^:])\/\/.*$/gm, "$1"); // [^:] 让 https:// 里的双斜杠不被当成行注释
    expect(code).not.toMatch(/(^|[^.\w])confirm\(/);
  });
});
