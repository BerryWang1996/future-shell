/**
 * 全局右键兜底：行为判据 + 接线守卫。
 *
 * 用户实测（2026-08-28）：「应用里右键总是弹出和浏览器一样的右键行为」。
 * 根因是右键此前只被**少数组件**显式处理，其余全漏给 WebView。
 */
import { describe, expect, it } from "vitest";
// ?raw 取源文本（同 action-wiring.test.ts / menus.test.ts 的既有口径）
import APP_SRC from "../App.svelte?raw";
import { shouldAllowNativeContextMenu } from "./context-menu";

describe("全局右键兜底：判定", () => {
  it.each(["INPUT", "TEXTAREA"])("%s 放行原生菜单（剪切/复制/粘贴是真有用的）", (tagName) => {
    expect(shouldAllowNativeContextMenu({ tagName })).toBe(true);
  });

  it("contenteditable 放行（富文本编辑区同理）", () => {
    expect(shouldAllowNativeContextMenu({ tagName: "DIV", isContentEditable: true })).toBe(true);
  });

  // 这几个是用户实际点到的地方：面板空白、按钮、列表项、画布
  it.each(["DIV", "BUTTON", "SPAN", "LI", "CANVAS", "BODY", "TABLE"])(
    "%s 吃掉浏览器菜单（用户不该在远程工具里看到「重新加载/检查」）",
    (tagName) => {
      expect(shouldAllowNativeContextMenu({ tagName })).toBe(false);
    },
  );

  // SELECT 故意不放行：下拉框的原生右键菜单是**浏览器**菜单不是编辑菜单。
  // 键盘那条口径（onWindowKeydown 的 inField）含 SELECT 是因为它要保原生
  // **键盘**行为（上下键选项），与右键无关——两条规则形似而实不同，
  // 这条判据把差异钉住，防止有人"统一"成同一个正则。
  it("SELECT 不放行（下拉框的原生右键是浏览器菜单，不是编辑菜单）", () => {
    expect(shouldAllowNativeContextMenu({ tagName: "SELECT" })).toBe(false);
  });

  it("target 信息缺失时按吃掉处理（默认安全）", () => {
    expect(shouldAllowNativeContextMenu({})).toBe(false);
  });
});

describe("全局右键兜底：接线", () => {
  // 承诺方与验收方必须是两个人（同 action-wiring.test.ts 的纪律）：
  // 判定函数写得再对，`<svelte:window>` 上没挂这一行就是零效果，
  // 而那种失效**没有任何测试会红**——用户重新报一次「右键还是浏览器菜单」
  // 才会被发现。
  it("App.svelte 的 svelte:window 上挂着 oncontextmenu", () => {
    const windowTag = APP_SRC.match(/<svelte:window[^>]*>/)?.[0] ?? "";
    expect(windowTag, "App.svelte 里找不到 <svelte:window>").not.toBe("");
    expect(windowTag, "全局右键兜底没接线——右键会继续弹浏览器菜单").toContain(
      "oncontextmenu",
    );
  });

  it("兜底处理器放行输入框（口径与 shouldAllowNativeContextMenu 一致）", () => {
    // 不比对实现细节，只确认它**用了**那个判定——避免 App 里另写一套走样的规则
    expect(
      APP_SRC.includes("isContentEditable") || APP_SRC.includes("shouldAllowNativeContextMenu"),
      "兜底处理器没有放行文本输入区：输入框里的复制/粘贴菜单会一起消失",
    ).toBe(true);
  });
});
