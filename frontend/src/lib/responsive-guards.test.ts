/**
 * 响应式结构守卫（路线图 4c「响应式逐组件核查」，2026-09-02）。
 *
 * 真正的布局核查在 `scripts/responsive-audit.mjs`——对着真跑的 WebView2 在 800×600 / 1000×700 /
 * 1280×800 三档逐状态量溢出并截图。jsdom 没有布局引擎，这里量不了像素；能守的是那次核查修掉的
 * **几类结构成因**，让它们不随下一个新对话框复制回来：
 *
 *  ① 对话框根盒必须有高度上限（max-height）——600px 高的窗口里没有它，长内容把底部按钮推出屏外；
 *     首轮静态清单里 13 个对话框缺这一条（AuthPrompt / HostKey / RdpCert / Confirm / TextInput / Vault /
 *     ConnectionDetail / DeleteConfirm / Zmodem / Sftp 符号链接 / McpConfirm / CloseConfirm / SessionRecovery）。
 *  ② 对话框根盒的固定像素宽必须配 max-width（或用 min()）——否则 800px 视口里贴边到边。
 *  ③ overlay 侧栏不得用 transform 收起——transform 让它成为后代 fixed 元素的包含块，侧栏里的右键菜单
 *     与遮罩被 240px 的 overflow:hidden 盒子裁到只剩一线（800×600 真机截图抓到）。
 *  ④ 贴右缘的右键菜单要能把子菜单翻到左侧（menu-pos.ts 写 data-sub-side，Sidebar 按它翻）。
 */
import fs from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import APP_SRC from "../App.svelte?raw";
import SIDEBAR_SRC from "../components/Sidebar.svelte?raw";
import MENU_POS_SRC from "./menu-pos.ts?raw";

const COMPONENTS = path.resolve(process.cwd(), "src/components");

/** `<style>` 里的顶层规则：[selector, body]。这些组件没有嵌套规则；@media 块整体跳过。 */
function rules(src: string): Array<[string, string]> {
  const m = /<style[^>]*>([\s\S]*?)<\/style>/.exec(src);
  if (!m) return [];
  const css = m[1].replace(/\/\*[\s\S]*?\*\//g, "").replace(/@media[^{]*\{[\s\S]*?\}\s*\}/g, "");
  const out: Array<[string, string]> = [];
  const re = /([^{}]+)\{([^{}]*)\}/g;
  let x: RegExpExecArray | null;
  while ((x = re.exec(css))) out.push([x[1].trim(), x[2]]);
  return out;
}

const DIALOG_ROOT = /^\.(dialog|panel|pop|confirm|dpanel|sym-dialog)$/;

describe("① ② 对话框根盒：有高度上限、固定宽配 max-width", () => {
  const files = fs.readdirSync(COMPONENTS).filter((f) => f.endsWith(".svelte"));
  const dialogFiles = files.filter((f) => fs.readFileSync(path.join(COMPONENTS, f), "utf8").includes('role="dialog"'));
  const roots: Array<{ file: string; selector: string; body: string }> = [];
  for (const f of dialogFiles) {
    for (const [sel, body] of rules(fs.readFileSync(path.join(COMPONENTS, f), "utf8"))) {
      // 根盒的识别：写了 width / min-width / max-width 任一的对话框根规则（只写 min/max 的也是根盒，如 CloseConfirm / SessionRecovery）
      if (DIALOG_ROOT.test(sel) && /(^|[^-])width:|min-width:|max-width:/.test(body)) roots.push({ file: f, selector: sel, body });
    }
  }

  it("非空证明：识别出了足够多的对话框根盒", () => {
    expect(dialogFiles.length).toBeGreaterThanOrEqual(25);
    expect(roots.length).toBeGreaterThanOrEqual(25);
  });

  it("每个根盒都有 max-height（600px 高的窗口里底部按钮要点得到）", () => {
    const missing = roots.filter((r) => !/max-height:/.test(r.body)).map((r) => `${r.file} ${r.selector}`);
    expect(missing, "补 `max-height: 92vh; overflow-y: auto;`（或同义）").toEqual([]);
  });

  it("固定像素宽的根盒都有 max-width 或 min() 上限（800px 视口里不贴边到边）", () => {
    const bad = roots
      .filter((r) => /(^|[^-])width:\s*\d+px/.test(r.body) && !/max-width:/.test(r.body))
      .map((r) => `${r.file} ${r.selector}`);
    expect(bad).toEqual([]);
    // min-width 也不能大过窄视口：min-width: 600px 在 800 宽里配 max-width: 800px 就是贴边
    const wideMin = roots
      .filter((r) => /min-width:\s*(\d+)px/.test(r.body) && Number(/min-width:\s*(\d+)px/.exec(r.body)![1]) > 400)
      .map((r) => `${r.file} ${r.selector}`);
    expect(wideMin, "min-width 超过 400px 请改成 min(Npx, 92vw)").toEqual([]);
  });
});

describe("③ overlay 侧栏的收起方式", () => {
  const overlayRule = /\.sidebar\.overlay \{([\s\S]*?)\}/.exec(APP_SRC.replace(/\/\*[\s\S]*?\*\//g, ""));

  it("规则存在且不用 transform 收起（transform 会把侧栏里的 fixed 弹层圈成只露一线）", () => {
    expect(overlayRule, "App.svelte 里找不到 .sidebar.overlay 规则").not.toBeNull();
    expect(overlayRule![1]).not.toMatch(/transform:/);
    expect(overlayRule![1]).not.toMatch(/translate:/);
  });

  it("用 left/right 偏移自身宽度收起，并同时 visibility: hidden（屏外的侧栏不该还能 Tab 到）", () => {
    expect(overlayRule![1]).toMatch(/left:\s*calc\(-1 \* var\(--fs-sidebar-w/);
    expect(overlayRule![1]).toMatch(/visibility:\s*hidden/);
    expect(APP_SRC, "aside 上要把宽度同步成 --fs-sidebar-w 自定义属性，CSS 的偏移量才跟得上拖拽出的宽度").toMatch(/style:--fs-sidebar-w="\{\$sidebarWidth\}px"/);
  });

  it("右停靠的两条规则都写了 left: auto（否则展开态被 left: 0 拉成通栏）", () => {
    const src = APP_SRC.replace(/\/\*[\s\S]*?\*\//g, "");
    expect(src).toMatch(/\.sidebar\.overlay\.dock-right \{[^}]*left:\s*auto/);
    expect(src).toMatch(/\.sidebar\.overlay\.dock-right\.overlay-open \{[^}]*left:\s*auto/);
  });

  it("侧栏里的弹层是 fixed 定位（它们依赖视口作包含块，这正是 ③ 禁用 transform 的原因）", () => {
    for (const [sel, body] of rules(SIDEBAR_SRC)) {
      if (sel === ".ctxmenu" || sel === ".menu-overlay") expect(body, sel).toMatch(/position:\s*fixed/);
    }
  });
});

describe("④ 贴右缘的右键菜单把子菜单翻到左侧", () => {
  it("clampToViewport 按平移后的右缘写 data-sub-side", () => {
    expect(MENU_POS_SRC).toMatch(/node\.dataset\.subSide\s*=\s*submenuSide\(/);
  });
  it("Sidebar 按 data-sub-side=left 把 .submenu 翻到左侧", () => {
    expect(SIDEBAR_SRC).toMatch(/\.ctxmenu\[data-sub-side="left"\] \.submenu \{[^}]*right:\s*100%/);
  });
});
