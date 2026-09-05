#!/usr/bin/env node
/**
 * forced-colors-audit.mjs — Windows 高对比度（forced-colors: active）核查（路线图 4c，2026-09-02）。
 *
 * # 量什么
 *
 * 强制颜色模式下浏览器把 `color` / `background-color` / `border-color` / `fill` / `stroke` 全部替换成
 * 系统调色板里的值，并把 `box-shadow` / `text-shadow` 抹成 none、非 <img> 的 `background-image` 抹成 none。
 * 后果是**一切只靠颜色（或只靠投影）区分的状态会塌成同一个样子**——绿灯与红灯长得一样、选中的标签与
 * 没选中的长得一样，而这些恰恰是用户判断「连上没有」「我在哪个会话」的唯一信号。
 *
 * 所以判据不是「有没有写 forced-colors 规则」（那只证明写过），而是：
 *   **把同一构件的各语义状态在强制颜色下的计算样式取出来两两比对；任意两个语义不同的状态算出
 *   完全相同的视觉签名 = 一处真缺陷**。签名含背景/前景/四边框色与线型/圆角/投影/轮廓/描边/尺寸。
 *   只报「常态分得清、强制颜色下分不清」的回退——常态就重复的状态是设计如此，不归本项。
 *
 * # 怎么造出各状态
 *
 * Svelte 的样式是散列作用域的（`.lamp.svelte-1abc`），凭空建的 `<div class="lamp">` 匹配不到任何规则。
 * 故一律**克隆页面上真实存在的宿主元素的散列类**、只换语义类——量到的就是产品规则本身。
 *
 * # 用法
 *   应用以 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=19333 启动后
 *   node scripts/forced-colors-audit.mjs [--port 19333] [--out target/forced-colors-audit]
 * 任一构件的任意两个状态不可辨 → 退出码 1。
 */
import fs from "node:fs";
import path from "node:path";
import { connect, screenshot as shoot, sleep } from "./lib/cdp.mjs";
import { js } from "./lib/app-driver.mjs";

const argv = process.argv.slice(2);
const opt = (name, def) => {
  const i = argv.indexOf(`--${name}`);
  if (i < 0) return def;
  const v = argv[i + 1];
  return v === undefined || v.startsWith("--") ? true : v;
};
const PORT = Number(opt("port", 19333));
const OUT = path.resolve(String(opt("out", "target/forced-colors-audit")));

/**
 * 构件表：宿主选择器（取散列作用域类用）、造哪个标签、基类、各语义状态类。
 * `indicator: true` = **纯指示器**（8px 小圆点，自身即全部视觉）——这类要额外过「画得出来吗」；
 * 行 / 按钮 / 标签不打这个标：它们靠内容（文字、图标）作画，自身没底色没边框是正常的。
 */
const WIDGETS = [
  // host 一律取**恒存在**的容器（Svelte 的散列类是按组件文件发的，同组件里任何元素都带同一个），
  // 不取 `.lamp` / `.tab` 这类要有数据才渲染的元素——否则没会话时整份核查静默跳过，看起来像全过。
  { name: "侧栏主机状态灯", host: '[data-testid="sidebar-tree"]', tag: "span", base: "lamp", indicator: true,
    states: ["connected", "connecting", "disconnected", "error", "probe-green", "probe-red", "probe-gray"] },
  { name: "侧栏行选中态", host: '[data-testid="sidebar-tree"]', tag: "div", base: "row leaf", states: ["", "active"] },
  { name: "标签页选中态", host: ".tabbar", tag: "button", base: "tab", states: ["", "active"] },
  { name: "标签状态点", host: ".tabbar", tag: "span", base: "status", indicator: true,
    states: ["connected", "connecting", "disconnected", "error"] },
  { name: "状态栏指示点", host: '[data-testid="statusbar"]', tag: "span", base: "dot", indicator: true, states: ["ok", "warn", "err"] },
  // pressed 不入表：ToolBar.svelte 注明按压态「与 primary 同源」，常态下本就同款，是设计不是缺陷。
  { name: "工具栏主按钮", host: '[data-testid="toolbar"]', tag: "button", base: "tbtn", states: ["", "primary"] },
  { name: "断线横幅", host: ".terminal-root", tag: "div", base: "disconnect-banner", states: ["", "gave-up"],
    need: "至少一个终端会话（TerminalPane 挂载）" },
];

/** 视觉签名：强制颜色下真正决定「看起来一不一样」的那些计算值。 */
const SIGNATURE_FN = [
  "(sel, tag, base, states, canvas) => {",
  "  const host = document.querySelector(sel);",
  "  if (!host) return { missing: true };",
  "  // 归一：全透明 与「与 Canvas 同色」在屏幕上无从分辨（透明叠在 Canvas 上就是 Canvas），",
  "  // 数值却是 rgba(0,0,0,0) !== rgb(0,0,0)。不归一就会把「选中行看不出来」判成通过。",
  '  const norm = (c) => (/,\\s*0\\)$/.test(c) || c === canvas ? "«canvas»" : c);',
  '  const scoped = [...host.classList].filter((c) => c.startsWith("svelte-"));',
  '  const box = document.createElement("div");',
  '  box.id = "__fc_probe";',
  '  box.style.cssText = "position:fixed;left:-9999px;top:0;width:200px";',
  "  (host.parentElement || document.body).appendChild(box);",
  "  const out = {};",
  "  for (const st of states) {",
  "    const el = document.createElement(tag);",
  '    el.className = [...base.split(" "), ...st.split(" ").filter(Boolean), ...scoped].join(" ");',
  "    box.appendChild(el);",
  "    const cs = getComputedStyle(el);",
  '    out[st || "(默认)"] = [',
  "      norm(cs.backgroundColor), cs.color, cs.backgroundImage,",
  "      cs.borderTopColor, cs.borderTopStyle, cs.borderTopWidth,",
  "      cs.borderRightColor, cs.borderRightStyle, cs.borderRightWidth,",
  "      cs.borderBottomColor, cs.borderBottomStyle, cs.borderBottomWidth,",
  "      cs.borderLeftColor, cs.borderLeftStyle, cs.borderLeftWidth,",
  "      cs.borderRadius, cs.boxShadow, cs.outlineColor, cs.outlineStyle, cs.outlineWidth,",
  "      cs.opacity, cs.fill, cs.stroke, cs.width, cs.height,",
  '    ].join(" | ");',
  "  }",
  "  box.remove();",
  "  return { sig: out };",
  "}",
].join("\n");

/** 自检：造一对必然相同、一对必然不同，探针要各自判对，否则整份报告不可信。 */
const SELF_TEST = [
  "(() => {",
  '  const box = document.createElement("div");',
  '  box.id = "__fc_selftest";',
  '  box.style.cssText = "position:fixed;left:-9999px;top:0";',
  '  const css = "#__fc_selftest .fcA,#__fc_selftest .fcB{background:red}"',
  '    + "#__fc_selftest .fcC{border:2px solid blue}#__fc_selftest .fcD{border:2px dashed blue}";',
  '  box.innerHTML = "<style>" + css + "</style><i class=\\"fcA\\"></i><i class=\\"fcB\\"></i>"',
  '    + "<i class=\\"fcC\\"></i><i class=\\"fcD\\"></i>";',
  "  document.body.appendChild(box);",
  '  const sig = (c) => { const cs = getComputedStyle(box.querySelector("." + c));',
  '    return [cs.backgroundColor, cs.borderTopColor, cs.borderTopStyle, cs.borderTopWidth, cs.boxShadow].join("|"); };',
  '  const r = { same: sig("fcA") === sig("fcB"), differ: sig("fcC") !== sig("fcD") };',
  "  box.remove();",
  "  return r;",
  "})()",
].join("\n");

const SVG_JS = [
  "(() => {",
  '  const p = document.querySelector(".icon path, .icon rect, .icon circle");',
  "  if (!p) return null;",
  "  const cs = getComputedStyle(p);",
  '  const host = getComputedStyle(p.closest("button") || p.parentElement);',
  "  return { stroke: cs.stroke, fill: cs.fill, hostColor: host.color, strokeWidth: cs.strokeWidth };",
  "})()",
].join("\n");

/** 当前模式下 `Canvas` 解析成什么（强制颜色下随系统主题变，必须现取）。 */
const CANVAS_JS = [
  "(() => {",
  '  const el = document.createElement("div");',
  '  el.style.cssText = "position:fixed;left:-9999px;background-color:Canvas";',
  "  document.body.appendChild(el);",
  "  const c = getComputedStyle(el).backgroundColor;",
  "  el.remove();",
  "  return c;",
  "})()",
].join("\n");

async function measure(cdp) {
  const rows = [];
  const canvas = await cdp.eval(CANVAS_JS);
  for (const w of WIDGETS) {
    const call = `(${SIGNATURE_FN})(${JSON.stringify(w.host)}, ${JSON.stringify(w.tag)}, ${JSON.stringify(w.base)}, ${JSON.stringify(w.states)}, ${JSON.stringify(canvas)})`;
    rows.push({ ...w, ...(await cdp.eval(call)) });
  }
  return rows;
}

/** 语义不同却视觉相同的状态对。 */
function collisions(sig) {
  const out = [];
  const keys = Object.keys(sig);
  for (let i = 0; i < keys.length; i++)
    for (let k = i + 1; k < keys.length; k++)
      if (sig[keys[i]] === sig[keys[k]]) out.push([keys[i], keys[k]]);
  return out;
}

async function main() {
  const cdp = await connect(PORT);
  fs.mkdirSync(OUT, { recursive: true });
  const report = { normal: null, forced: null, svg: {}, findings: [] };
  let failed = 0;
  try {
    await cdp.eval(js.closeAll);
    // 两个方向都强制，不受本机是否真开着高对比度左右
    await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "none" }] });
    await sleep(250);
    report.normal = await measure(cdp);
    report.svg.normal = await cdp.eval(SVG_JS);

    await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "active" }] });
    await sleep(300);
    const on = await cdp.eval('matchMedia("(forced-colors: active)").matches');
    console.log(`仿真 forced-colors: active 生效 = ${on}`);
    if (on !== true) throw new Error("forced-colors 仿真没生效，报告不可信");
    const st = await cdp.eval(SELF_TEST);
    console.log(`探针自检：同色判同 = ${st.same}，异线型判异 = ${st.differ}`);
    if (!st.same || !st.differ) throw new Error("探针自检失败——签名判据失灵，报告不可信");
    report.forced = await measure(cdp);
    report.svg.forced = await cdp.eval(SVG_JS);
    await shoot(cdp, path.join(OUT, "forced-colors.png"), OUT);

    console.log("");
    for (let i = 0; i < report.forced.length; i++) {
      const f = report.forced[i];
      const n = report.normal[i];
      if (f.missing) {
        console.log(`skip  ${f.name}  — 页面上没有宿主 ${f.host}${f.need ? `（需要：${f.need}）` : ""}`);
        continue;
      }
      // 「画得出来吗」：底色归一成 «canvas»（透明或与 Canvas 同色）、四边框宽全 0、轮廓宽也是 0
      // = 屏幕上什么都没有。这条必须与两两比对并列——`background: currentColor` 被强制成 Canvas 后，
      // 两颗都画不出来的点仍会因为圆角不同被判「可辨」，而用户面前是两处空白（实测踩中，方案因此
      // 改用系统色关键字）。**只对纯指示器成立**：行/按钮/标签靠内容作画，自身空白是常态。
      const invisible = !f.indicator ? [] : Object.entries(f.sig)
        .filter(([, v]) => {
          const c = v.split(" | ");
          return c[0] === "«canvas»"
            && [c[5], c[8], c[11], c[14]].every((w) => parseFloat(w) === 0)
            && parseFloat(c[19]) === 0;
        })
        .map(([k]) => k);
      if (invisible.length > 0) {
        failed++;
        console.log(`\x1b[31mFAIL\x1b[0m  ${f.name}  — 这些状态在强制颜色下**画不出来**（底色被强制成 Canvas 且无边框）：${invisible.join("、")}`);
        for (const k of invisible) report.findings.push({ widget: f.name, host: f.host, state: k, phase: "invisible", signature: f.sig[k] });
        continue;
      }
      const colForced = collisions(f.sig);
      const colNormal = n.missing ? [] : collisions(n.sig);
      // 前置闸：常态下就分不清，说明**探针根本没匹配到组件规则**（散列作用域取错宿主是最常见的原因），
      // 或者这个构件的状态本来就没做区分。两种都是缺陷，不能靠「只报回退」的过滤悄悄变成 PASS——
      // 首版取 `.sidebar` 当宿主时命中了 App.svelte 的同名元素，三个构件因此判成假绿。
      if (colNormal.length > 0) {
        failed++;
        console.log(`[31mFAIL[0m  ${f.name}  — **常态下**就有 ${colNormal.length} 对状态视觉相同：探针没匹配到组件规则（宿主 ${f.host} 的散列作用域取错？），或该构件本就没区分状态`);
        for (const [a, b] of colNormal) report.findings.push({ widget: f.name, host: f.host, a, b, phase: "normal", signature: n.sig[a] });
        continue;
      }
      const regressions = colForced.filter(([a, b]) => !colNormal.some(([x, y]) => x === a && y === b));
      if (regressions.length === 0) {
        console.log(`PASS  ${f.name}  — ${Object.keys(f.sig).length} 个状态两两可辨`);
        continue;
      }
      failed++;
      console.log(`\x1b[31mFAIL\x1b[0m  ${f.name}  — 强制颜色下这些状态塌成同一个样子：`);
      for (const [a, b] of regressions) {
        console.log(`        「${a}」≡「${b}」   ${f.sig[a].split(" | ").slice(0, 3).join(" / ")}`);
        report.findings.push({ widget: f.name, host: f.host, a, b, signature: f.sig[a] });
      }
    }
    const sn = report.svg.normal;
    const sf = report.svg.forced;
    if (sn && sf) {
      const ok = sf.stroke !== "none" && sf.stroke !== sf.fill;
      console.log(`${ok ? "PASS" : "\x1b[31mFAIL\x1b[0m"}  SVG 图标描边在强制颜色下仍在  — 常态 stroke=${sn.stroke} → 强制 stroke=${sf.stroke}, fill=${sf.fill}`);
      if (!ok) {
        failed++;
        report.findings.push({ widget: "SVG 图标", detail: sf });
      }
    }
  } finally {
    try {
      await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "" }] });
      await cdp.eval(js.closeAll);
    } catch { /* 收尾尽力而为 */ }
    cdp.close();
  }
  fs.writeFileSync(path.join(OUT, "report.json"), JSON.stringify(report, null, 2));
  console.log(`\n${failed} 个构件在强制颜色下不可辨 → ${path.join(OUT, "report.json")}`);
  process.exit(failed ? 1 : 0);
}

main().catch((e) => { console.error("forced-colors-audit 失败:", e.message); process.exit(1); });
