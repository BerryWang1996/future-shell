#!/usr/bin/env node
/**
 * responsive-audit.mjs — 响应式逐组件核查（路线图 4c，2026-09-02）。
 *
 * 对着**真跑着的应用**（WebView2 + CDP）在若干视口尺寸下逐个界面状态做三件事：
 *   ① 量：找出真实溢出——超出视口的元素、出现横向滚动条的容器、nowrap 文字被裁且无省略号、
 *      比视口还高/宽的对话框；结果带「组件路径 + 尺寸 + 数字」，直接就是路线图要的清单格式。
 *   ② 拍：每个尺寸 × 状态一张截图，作为基线留档（target/responsive-audit/<size>/<state>.png）。
 *   ③ 汇：report.json + report.md。
 *
 * 为什么不用 jsdom 组件测试：jsdom 没有布局引擎，`getBoundingClientRect` 恒为 0，测不出溢出。
 * 为什么用 CDP 的 Emulation 而不是真改窗口尺寸：窗口 set_size 要走 ACL（capabilities 未放行），
 * 而 `Emulation.setDeviceMetricsOverride` 直接改布局视口，`matchMedia`（侧栏窄屏断点）同样跟着变。
 *
 * 用法（Node ≥ 22，零依赖，WebSocket 是内建全局）：
 *   1. 以调试口启动应用：环境变量 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=19333
 *      （9333 常落在 Windows 保留端口段里，连不上先查 `netsh interface ipv4 show excludedportrange protocol=tcp`）。
 *   2. node scripts/responsive-audit.mjs [--port 19333] [--sizes 800x600,1000x700,1280x800]
 *                                        [--out target/responsive-audit] [--states idle,settings,...] [--no-shots]
 *
 * 判读口径：
 *   - overflow-right / overflow-left / overflow-bottom：元素可见矩形超出视口，且没有任何祖先能滚动/裁切它 → 真溢出。
 *   - hscroll：overflow-x 为 auto/scroll 的容器实际出现了横向滚动（scrollWidth > clientWidth）。
 *   - nowrap-clip：white-space: nowrap 的叶子文字比自己的盒子宽，又没有 text-overflow: ellipsis → 文字被硬裁。
 *   - dialog-taller/wider-than-viewport：对话框比视口还大 → 底部按钮点不到。
 *   - popup-clipped：fixed/absolute 弹层被某个 overflow:hidden 的祖先裁掉（典型成因：祖先带 transform，
 *     成了 fixed 元素的包含块）——比溢出更隐蔽，它根本不显示。
 *   - 刻意藏在屏外的 absolute/fixed + transform 元素、visibility:hidden 的元素不计。
 *   - 每次运行先做探针自检（塞 6 个必违规的对照元素，必须全部抓到），零发现的报告才可信。
 */
import fs from "node:fs";
import path from "node:path";
import { connect, screenshot as shoot, sleep } from "./lib/cdp.mjs";
import { js } from "./lib/app-driver.mjs";

// ── 参数 ────────────────────────────────────────────────────────────────────────
const argv = process.argv.slice(2);
const opt = (name, def) => {
  const i = argv.indexOf(`--${name}`);
  if (i < 0) return def;
  const v = argv[i + 1];
  return v === undefined || v.startsWith("--") ? true : v;
};
const PORT = Number(opt("port", 19333));
const SIZES = String(opt("sizes", "800x600,1000x700,1280x800")).split(",").map((s) => s.split("x").map(Number));
const OUT = path.resolve(String(opt("out", "target/responsive-audit")));
const ONLY = opt("states", null) ? String(opt("states")).split(",") : null;
const SHOTS = opt("no-shots", false) !== true;

// CDP 客户端见 scripts/lib/cdp.mjs（与 motion-audit.mjs 共用）。

// ── 页内探针 ─────────────────────────────────────────────────────────────────────
const PROBE = String.raw`(() => {
  const vw = document.documentElement.clientWidth, vh = document.documentElement.clientHeight;
  const issues = [];
  const label = (el) => {
    const parts = []; let e = el;
    while (e && e !== document.body && parts.length < 4) {
      let s = e.tagName.toLowerCase();
      const tid = e.getAttribute && e.getAttribute("data-testid");
      if (e.id) s += "#" + e.id;
      else if (tid) s += "[" + tid + "]";
      else if (typeof e.className === "string" && e.className) {
        const c = e.className.trim().split(/\s+/).filter((x) => !x.startsWith("svelte-")).slice(0, 2).join(".");
        if (c) s += "." + c;
      }
      parts.unshift(s); e = e.parentElement;
    }
    return parts.join(" > ");
  };
  // 只认**非根**的裁切/滚动祖先：styles.css 给 html/body 写了 overflow-x: hidden，把 body 算进去
  // 会让「超出视口」永远判不出来（首版探针正是这样 107 个状态点零发现——被 body 一刀裁掉的溢出
  // 恰恰就是要抓的事故）。
  const clipAncestor = (el, axis) => {
    for (let p = el.parentElement; p && p !== document.body && p !== document.documentElement; p = p.parentElement) {
      const o = getComputedStyle(p)[axis === "x" ? "overflowX" : "overflowY"];
      if (o === "auto" || o === "scroll" || o === "hidden" || o === "clip") return p;
    }
    return null;
  };
  // fixed/absolute 元素的**包含块**：transform 一族（transform/translate/rotate/scale/perspective/filter/
  // backdrop-filter/contain:paint/will-change:transform）会把 fixed 元素圈住；absolute 还认最近的定位祖先。
  const TRANSFORMISH = (c) => c.transform !== "none" || (c.translate && c.translate !== "none") || (c.rotate && c.rotate !== "none") || (c.scale && c.scale !== "none") || c.perspective !== "none" || (c.filter && c.filter !== "none") || (c.backdropFilter && c.backdropFilter !== "none") || /paint|layout|strict|content/.test(c.contain || "") || /transform/.test(c.willChange || "");
  const containingBlock = (el, pos) => {
    for (let p = el.parentElement; p && p !== document.documentElement; p = p.parentElement) {
      const c = getComputedStyle(p);
      if (TRANSFORMISH(c)) return p;
      if (pos === "absolute" && c.position !== "static") return p;
    }
    return null; // 视口（初始包含块）
  };
  // 真能裁到它的祖先 = 沿**包含块链**往上走时遇到的 overflow 非 visible 的节点。
  // fixed/absolute 元素直接跳到自己的包含块，夹在中间的 overflow:hidden 祖先裁不到它（CSS 规则，
  // 也是 1280 停靠形态下右键菜单能伸出 240px 侧栏的原因）；包含块是视口的 fixed 元素再往上没人裁得到。
  // 首版探针把「最近的 overflow:hidden 祖先」一律当裁切者，在 1280 上报出了并不存在的裁切。
  const clippers = (el, axis) => {
    const out = [];
    let node = el;
    while (node && node !== document.body && node !== document.documentElement) {
      const c = getComputedStyle(node);
      let next;
      if (c.position === "fixed" || c.position === "absolute") {
        next = containingBlock(node, c.position);
        if (next === null) break; // 视口作包含块
      } else {
        next = node.parentElement;
      }
      if (!next || next === document.body || next === document.documentElement) break;
      const o = getComputedStyle(next)[axis === "x" ? "overflowX" : "overflowY"];
      if (o !== "visible") out.push(next);
      node = next;
    }
    return out;
  };
  /** 元素是否被某个真裁切/滚动容器管着（据此决定「超出视口」算不算事故）。 */
  const contained = (el, cs, axis) => (cs.position === "fixed" || cs.position === "absolute") ? clippers(el, axis).length > 0 : !!clipAncestor(el, axis);
  const rnd = (n) => Math.round(n);
  const rect = (r) => [rnd(r.left), rnd(r.top), rnd(r.width), rnd(r.height)];
  if (document.documentElement.scrollWidth > vw + 1) issues.push({ kind: "root-hscroll", el: "html", detail: document.documentElement.scrollWidth + " > " + vw });
  if (document.body.scrollWidth > vw + 1) issues.push({ kind: "root-hscroll", el: "body", detail: document.body.scrollWidth + " > " + vw + "（被 overflow-x:hidden 硬裁）" });
  for (const el of document.querySelectorAll("body *")) {
    if (el.closest("#__ra_ctrl") && !window.__ra_selftest) continue;
    const cs = getComputedStyle(el);
    if (cs.display === "none" || cs.visibility === "hidden" || cs.opacity === "0") continue;
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;
    const offscreenByDesign = cs.transform !== "none" && (cs.position === "absolute" || cs.position === "fixed");
    if (!offscreenByDesign) {
      if (r.right > vw + 1 && !contained(el, cs, "x")) issues.push({ kind: "overflow-right", el: label(el), rect: rect(r), detail: "right=" + rnd(r.right) + " > vw=" + vw });
      if (r.left < -1 && !contained(el, cs, "x")) issues.push({ kind: "overflow-left", el: label(el), rect: rect(r), detail: "left=" + rnd(r.left) });
      if (r.bottom > vh + 1 && !contained(el, cs, "y")) issues.push({ kind: "overflow-bottom", el: label(el), rect: rect(r), detail: "bottom=" + rnd(r.bottom) + " > vh=" + vh });
    }
    if ((cs.overflowX === "auto" || cs.overflowX === "scroll") && el.clientWidth > 0 && el.scrollWidth > el.clientWidth + 1)
      issues.push({ kind: "hscroll", el: label(el), rect: rect(r), detail: el.scrollWidth + " > " + el.clientWidth });
    // 弹层（fixed/absolute）被某个 overflow: hidden 的祖先裁掉：比「溢出视口」更隐蔽——它根本不显示。
    // 典型成因是祖先带 transform（成为 fixed 元素的包含块），2026-09-02 在 overlay 侧栏的右键菜单上抓到。
    if ((cs.position === "fixed" || cs.position === "absolute") && !offscreenByDesign) {
      for (const clip of clippers(el, "x")) {
        const co = getComputedStyle(clip).overflowX, cr = clip.getBoundingClientRect();
        if ((co === "hidden" || co === "clip") && (r.right > cr.right + 1 || r.left < cr.left - 1)) {
          issues.push({ kind: "popup-clipped", el: label(el), rect: rect(r), detail: "被 " + label(clip) + " 裁掉 " + rnd(Math.max(r.right - cr.right, cr.left - r.left)) + "px" });
          break;
        }
      }
    }
    if (cs.whiteSpace === "nowrap" && el.children.length === 0 && el.scrollWidth > el.clientWidth + 1 && cs.textOverflow !== "ellipsis" && cs.overflowX !== "hidden" && cs.overflowX !== "clip")
      issues.push({ kind: "nowrap-clip", el: label(el), rect: rect(r), detail: el.scrollWidth + " > " + el.clientWidth + " 「" + (el.textContent || "").trim().slice(0, 40) + "」" });
  }
  for (const d of document.querySelectorAll('[role="dialog"], [role="alertdialog"]')) {
    const r = d.getBoundingClientRect();
    if (r.height > vh - 4) issues.push({ kind: "dialog-taller-than-viewport", el: label(d), rect: rect(r), detail: rnd(r.height) + " vs vh=" + vh });
    if (r.width > vw - 4) issues.push({ kind: "dialog-wider-than-viewport", el: label(d), rect: rect(r), detail: rnd(r.width) + " vs vw=" + vw });
  }
  const map = new Map();
  for (const i of issues) { const k = i.kind + "|" + i.el; if (map.has(k)) map.get(k).count++; else map.set(k, { ...i, count: 1 }); }
  return { vw, vh, dialogs: document.querySelectorAll('[role="dialog"]').length, issues: [...map.values()] };
})()`;

/**
 * 探针自检（每次运行开头都跑，`--no-self-test` 可跳）：往页面里塞四个**必然违规**的对照元素，
 * 探针必须逐个抓到，否则整份「零发现」不可信。首版探针就是在这里被抓出「body 的 overflow-x:hidden
 * 让超出视口永远判不出」的——零发现的报告与探针失明的报告长得一模一样，只有对照能分辨。
 */
const SELF_TEST_INJECT = String.raw`(() => {
  const vw = document.documentElement.clientWidth, vh = document.documentElement.clientHeight;
  document.getElementById("__ra_ctrl")?.remove();
  const c = document.createElement("div"); c.id = "__ra_ctrl";
  c.innerHTML =
    '<div id="__ra_wide" style="position:absolute;left:0;top:0;width:' + (vw + 300) + 'px;height:4px"></div>' +
    '<div id="__ra_scroll" style="position:absolute;left:0;top:10px;width:120px;height:20px;overflow-x:auto"><div style="width:400px;height:4px"></div></div>' +
    '<div id="__ra_nowrap" style="position:absolute;left:0;top:40px;width:40px;white-space:nowrap">这是一段肯定超出四十像素盒子的很长很长的文字</div>' +
    '<div id="__ra_tall" role="dialog" style="position:absolute;left:0;top:0;width:10px;height:' + (vh + 50) + 'px"></div>' +
    '<div id="__ra_clipbox" style="position:absolute;left:0;top:70px;width:50px;height:20px;overflow:hidden"><div id="__ra_popup" style="position:absolute;left:0;top:0;width:200px;height:10px"></div></div>' +
    // fixed 弹层被带 transform 的 overflow:hidden 祖先圈住 → 必须抓到（overlay 侧栏那一类）
    '<div id="__ra_tbox" style="position:absolute;left:0;top:100px;width:50px;height:20px;overflow:hidden;transform:translateX(0)"><div id="__ra_fixedpop" style="position:fixed;left:0;top:0;width:200px;height:10px"></div></div>' +
    // 反例：fixed 弹层的祖先只有 overflow:hidden、没有 transform → 裁不到它，不得误报（1280 停靠形态）
    '<div id="__ra_nbox" style="position:absolute;left:0;top:130px;width:50px;height:20px;overflow:hidden"><div id="__ra_freefixed" style="position:fixed;left:0;top:0;width:200px;height:10px"></div></div>';
  document.body.appendChild(c);
  window.__ra_selftest = true;
  return "injected";
})()`;
const SELF_TEST_CLEANUP = String.raw`(() => { document.getElementById("__ra_ctrl")?.remove(); window.__ra_selftest = false; return "cleaned"; })()`;

async function selfTest(cdp) {
  await cdp.eval(SELF_TEST_INJECT);
  let probe;
  try { probe = await cdp.eval(PROBE); } finally { await cdp.eval(SELF_TEST_CLEANUP); }
  const has = (kind, id) => probe.issues.some((i) => i.kind === kind && i.el.includes(id));
  const expect = [
    ["overflow-right", "__ra_wide"],
    ["hscroll", "__ra_scroll"],
    ["nowrap-clip", "__ra_nowrap"],
    ["dialog-taller-than-viewport", "__ra_tall"],
    ["overflow-bottom", "__ra_tall"],
    ["popup-clipped", "__ra_popup"],
    ["popup-clipped", "__ra_fixedpop"],
  ];
  const missed = expect.filter(([k, id]) => !has(k, id));
  const falsePositive = has("popup-clipped", "__ra_freefixed");
  if (missed.length || falsePositive) {
    console.error("自检时探针实际返回：", JSON.stringify(probe.issues.map((i) => `${i.kind} ${i.el}`), null, 0));
    if (missed.length) throw new Error("探针自检失败，抓不到对照元素：" + missed.map(([k, id]) => `${k}(${id})`).join(", ") + "——报告不可信，终止");
    throw new Error("探针自检失败：把不受裁切的 fixed 弹层（__ra_freefixed）误报成 popup-clipped——报告不可信，终止");
  }
  console.log(`探针自检通过：${expect.length} 个对照违规全部抓到，1 个反例未误报`);
}

// ── 页内动作 ─────────────────────────────────────────────────────────────────────
// 页内动作表见 scripts/lib/app-driver.mjs（与 motion-audit.mjs 共用）。

/** 状态表：name → 打开/关闭动作。`tabs: true` 表示打开后逐个标签页各量一次。 */
const STATES = [
  { name: "idle" },
  { name: "menu-tools", open: js.clickMenu("工具", null) },
  { name: "toolbar-menu", open: js.contextMenu('[data-testid="toolbar"]', "right"), close: js.closeCtx(".tb-scrim") },
  { name: "sidebar-ctxmenu", open: js.contextMenu(".row.leaf", "right"), close: js.closeCtx(".menu-overlay") },
  // 右停靠 / 靠右元素的右键：菜单被钳回视口后，「移动到…」的子菜单若仍向右展开就出界
  { name: "sidebar-ctxmenu-edge", open: js.contextMenu(".row.leaf", "edge"), close: js.closeCtx(".menu-overlay") },
  { name: "sidebar-overlay-open", open: js.key("S", "KeyS", { ctrl: true, shift: true }), close: js.key("S", "KeyS", { ctrl: true, shift: true }), onlyIfOverlay: true },
  { name: "settings", open: js.clickMenu("工具", "选项"), tabs: true },
  { name: "profile-new", open: js.clickMenu("文件", "新建会话"), tabs: true },
  { name: "folder-new", open: js.clickMenu("文件", "新建文件夹") },
  { name: "import-foreign", open: js.clickMenu("文件", "导入(Xshell") },
  { name: "vault-manager", open: js.clickMenu("工具", "保险库管理") },
  { name: "quick-commands-editor", open: js.clickMenu("工具", "快速命令集编辑器") },
  { name: "quickbar", open: js.clickMenu("工具", "显示/隐藏快速命令条"), close: js.clickMenu("工具", "显示/隐藏快速命令条") },
  { name: "highlight", open: js.clickMenu("工具", "高亮关键字") },
  { name: "tunnels", open: js.clickMenu("工具", "隧道管理器") },
  { name: "history", open: js.clickMenu("工具", "历史命令") },
  { name: "schedule", open: js.clickMenu("工具", "计划任务") },
  { name: "replay", open: js.clickMenu("工具", "会话回放") },
  { name: "scheme-editor", open: js.clickMenu("工具", "配色方案编辑器") },
  { name: "key-manager", open: js.clickMenu("工具", "密钥/代理管理器") },
  { name: "ai", open: js.clickMenu("工具", "AI 助手") },
  { name: "audit", open: js.clickMenu("工具", "审计链校验") },
  { name: "agent", open: js.clickMenu("工具", "AI Agent") },
  { name: "monitor", open: js.clickMenu("视图", "监控抽屉"), close: js.clickMenu("视图", "监控抽屉") },
];

// ── 主流程 ───────────────────────────────────────────────────────────────────────
async function main() {
  const cdp = await connect(PORT);
  fs.mkdirSync(OUT, { recursive: true });
  const report = [];
  try {
    if (opt("no-self-test", false) !== true) {
      await cdp.send("Emulation.setDeviceMetricsOverride", { width: SIZES[0][0], height: SIZES[0][1], deviceScaleFactor: 1, mobile: false });
      await sleep(300);
      await selfTest(cdp);
    }
    for (const [w, h] of SIZES) {
      const sizeTag = `${w}x${h}`;
      const dir = path.join(OUT, sizeTag);
      fs.mkdirSync(dir, { recursive: true });
      await cdp.send("Emulation.setDeviceMetricsOverride", { width: w, height: h, deviceScaleFactor: 1, mobile: false });
      await sleep(400);
      await cdp.eval(js.closeAll);
      const isOverlay = await cdp.eval(js.overlay);
      for (const st of STATES) {
        if (ONLY && !ONLY.includes(st.name)) continue;
        if (st.onlyIfOverlay && !isOverlay) continue;
        let opened = "n/a";
        if (st.open) { opened = await cdp.eval(st.open); await sleep(350); }
        const variants = [];
        if (st.tabs) {
          const tabs = await cdp.eval(js.tabs);
          for (let i = 0; i < tabs.length; i++) {
            await cdp.eval(js.clickTab(i)); await sleep(250);
            variants.push({ suffix: `-${i}-${tabs[i].replace(/[^\w一-鿿]+/g, "")}`, tab: tabs[i] });
          }
          if (!tabs.length) variants.push({ suffix: "", tab: null });
          // 逐标签页回放：上面只是收集名字，这里真正逐个点开并量
          for (const v of variants) {
            if (v.tab !== null) { await cdp.eval(js.clickTab(variants.indexOf(v))); await sleep(300); }
            const probe = await cdp.eval(PROBE);
            const shot = SHOTS ? await screenshot(cdp, path.join(dir, `${st.name}${v.suffix}.png`)) : null;
            report.push({ size: sizeTag, state: st.name + (v.tab ? ` / ${v.tab}` : ""), opened, ...probe, shot });
            log(sizeTag, st.name + (v.tab ? ` / ${v.tab}` : ""), opened, probe);
          }
        } else {
          const probe = await cdp.eval(PROBE);
          const shot = SHOTS ? await screenshot(cdp, path.join(dir, `${st.name}.png`)) : null;
          report.push({ size: sizeTag, state: st.name, opened, ...probe, shot });
          log(sizeTag, st.name, opened, probe);
        }
        if (st.open) { await cdp.eval(st.close ?? js.closeAll); await sleep(250); await cdp.eval(js.closeAll); }
      }
    }
  } finally {
    try { await cdp.send("Emulation.clearDeviceMetricsOverride"); await cdp.eval(js.closeAll); } catch { /* 收尾尽力而为 */ }
    cdp.close();
  }
  fs.writeFileSync(path.join(OUT, "report.json"), JSON.stringify(report, null, 2));
  fs.writeFileSync(path.join(OUT, "report.md"), markdown(report));
  const total = report.reduce((n, r) => n + r.issues.length, 0);
  console.log(`\n共 ${report.length} 个状态点，${total} 条发现 → ${path.join(OUT, "report.md")}`);
}

const screenshot = (cdp, file) => shoot(cdp, file, OUT);

function log(size, state, opened, probe) {
  const n = probe.issues.length;
  const head = `[${size}] ${state.padEnd(34)} opened=${String(opened).padEnd(9)} dialogs=${probe.dialogs} 发现=${n}`;
  console.log(n ? `\x1b[31m${head}\x1b[0m` : head);
  for (const i of probe.issues) console.log(`    · ${i.kind.padEnd(28)} ${i.el}  ${i.detail}${i.count > 1 ? `  ×${i.count}` : ""}`);
}

function markdown(report) {
  const lines = ["# 响应式核查报告", "", `生成时间：${new Date().toISOString()}`, "", "| 尺寸 | 状态 | 打开 | 种类 | 元素 | 数值 | 次数 |", "|---|---|---|---|---|---|---|"];
  for (const r of report) {
    if (!r.issues.length) { lines.push(`| ${r.size} | ${r.state} | ${r.opened} | — | — | 无发现 | |`); continue; }
    for (const i of r.issues) lines.push(`| ${r.size} | ${r.state} | ${r.opened} | ${i.kind} | \`${i.el}\` | ${i.detail} | ${i.count} |`);
  }
  return lines.join("\n") + "\n";
}

main().catch((e) => { console.error("audit 失败:", e.message); process.exit(1); });
