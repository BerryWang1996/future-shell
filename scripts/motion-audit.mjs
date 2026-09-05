#!/usr/bin/env node
/**
 * motion-audit.mjs — 动效核查（路线图 4c「动效补齐」，2026-09-02）。
 *
 * 对着真跑的应用（WebView2 + CDP）量三件事：
 *   ① 引擎能力：@starting-style / :has() 是否可用，动效 token 是否落到了 :root；
 *   ② 每类进场对象的计算样式里确实挂上了 transition（对话框 + 遮罩 / 菜单 / 标签 / 监控抽屉 / toast 进场退场）；
 *   ③ **prefers-reduced-motion 仿真**（Emulation.setEmulatedMedia）：正向强制 no-preference 量到动效，
 *      反向强制 reduce 量到全部清零、toast 退场不再等那一拍——这是路线图给动效这一项定的硬约束，
 *      「别绕过既有机制」。两个方向都强制，是因为本机 Windows 关着「动画效果」，默认就是 reduce：
 *      不强制的话正向判据在这台机器上永远量不到东西。
 *
 * 为什么不在 jsdom 里测：jsdom 不解析 @starting-style、不算 transition，`getComputedStyle` 拿不到这些。
 *
 * 用法：应用以 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=19333 启动后
 *   node scripts/motion-audit.mjs [--port 19333]
 * 任一项 FAIL 退出码 1。
 */
import { connect, sleep } from "./lib/cdp.mjs";
import { js } from "./lib/app-driver.mjs";

const argv = process.argv.slice(2);
const PORT = Number(argv[argv.indexOf("--port") + 1] || 19333);

const results = [];
const check = (name, ok, detail = "") => {
  results.push({ name, ok, detail });
  console.log(`${ok ? "PASS" : "\x1b[31mFAIL\x1b[0m"}  ${name}${detail ? `  — ${detail}` : ""}`);
};
const j = (v) => JSON.stringify(v);

const Q = {
  caps: String.raw`(() => {
    const root = getComputedStyle(document.documentElement);
    let startingRules = 0;
    for (const ss of document.styleSheets) { try { for (const r of ss.cssRules) if (typeof CSSStartingStyleRule !== "undefined" && r instanceof CSSStartingStyleRule) startingRules++; } catch {} }
    return { startingSupported: typeof CSSStartingStyleRule !== "undefined", startingRules, has: CSS.supports("selector(:has(a))"),
      base: root.getPropertyValue("--fs-motion-base").trim(), fast: root.getPropertyValue("--fs-motion-fast").trim(),
      reduced: matchMedia("(prefers-reduced-motion: reduce)").matches, chrome: (navigator.userAgent.match(/Chrome\/[\d.]+/) || [""])[0] };
  })()`,
  dialog: String.raw`(() => {
    const d = document.querySelector('[role="dialog"]'); if (!d) return null;
    const cs = getComputedStyle(d), p = d.parentElement, ps = getComputedStyle(p);
    return { tp: cs.transitionProperty, td: cs.transitionDuration, ptp: ps.transitionProperty, ptd: ps.transitionDuration, parent: p.className.replace(/svelte-\w+/g, "").trim() };
  })()`,
  menu: String.raw`(() => { const m = document.querySelector('[role="menu"]'); if (!m) return null; const cs = getComputedStyle(m); return { tp: cs.transitionProperty, td: cs.transitionDuration }; })()`,
  tab: String.raw`(() => { const t = document.querySelector('[role="dialog"] [role="tab"]'); if (!t) return null; const cs = getComputedStyle(t); return { tp: cs.transitionProperty, td: cs.transitionDuration }; })()`,
  monitor: String.raw`(() => { const m = document.querySelector('[data-testid="monitor"]'); if (!m) return null; const cs = getComputedStyle(m); return { tp: cs.transitionProperty, td: cs.transitionDuration, h: Math.round(m.getBoundingClientRect().height) }; })()`,
  toastShow: String.raw`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    document.querySelector('[data-testid="tbtn-edit.screenshotCopy"]')?.click();
    await wait(250);
    const t = document.querySelector('[data-testid^="toast-"]'); if (!t) return null;
    const cs = getComputedStyle(t);
    return { id: t.dataset.testid, anim: cs.animationName, dur: cs.animationDuration, text: (t.textContent || "").trim().slice(0, 40) };
  })()`,
  toastDismiss: (id) => String.raw`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const t = document.querySelector('[data-testid="${id}"]'); if (!t) return null;
    t.querySelector("button")?.click();
    await wait(20);
    const afterClick = { leaving: t.classList.contains("leaving"), inDom: document.contains(t), opacity: getComputedStyle(t).opacity, td: getComputedStyle(t).transitionDuration };
    await wait(320);
    return { afterClick, goneLater: !document.contains(t) };
  })()`,
  clearToasts: String.raw`(async () => { for (const b of document.querySelectorAll('[data-testid^="toast-"] button')) b.click(); await new Promise((r) => setTimeout(r, 400)); return document.querySelectorAll('[data-testid^="toast-"]').length; })()`,
};

async function run(cdp, reduced) {
  const tag = reduced ? "【reduce】" : "";
  const want = (dur) => (reduced ? "0s" : dur);
  // 对话框 + 遮罩
  await cdp.eval(js.clickMenu("工具", "选项")); await sleep(300);
  const d = await cdp.eval(Q.dialog);
  check(`${tag}对话框 transition ${reduced ? "清零" : "= opacity+transform 140ms"}`, !!d && (reduced ? d.td.split(", ").every((x) => x === "0s") : /opacity/.test(d.tp) && /transform/.test(d.tp) && /0\.14s/.test(d.td)), j(d));
  check(`${tag}遮罩 transition ${reduced ? "清零" : "= opacity 140ms"}`, !!d && (reduced ? d.ptd.split(", ").every((x) => x === "0s") : /opacity/.test(d.ptp) && /0\.14s/.test(d.ptd)), d ? `parent=${d.parent} ${d.ptp} ${d.ptd}` : "无对话框");
  const t = await cdp.eval(Q.tab);
  check(`${tag}标签 [role=tab] transition ${reduced ? "清零" : "含 background-color/color/box-shadow 80ms"}`, !!t && (reduced ? t.td.split(", ").every((x) => x === "0s") : /background-color/.test(t.tp) && /box-shadow/.test(t.tp) && /0\.08s/.test(t.td)), j(t));
  await cdp.eval(js.closeAll); await sleep(200);
  // 菜单
  await cdp.eval(js.clickMenu("工具", null)); await sleep(150);
  const m = await cdp.eval(Q.menu);
  check(`${tag}菜单 [role=menu] transition ${reduced ? "清零" : "= opacity 80ms"}`, !!m && (reduced ? m.td === "0s" : m.tp === "opacity" && m.td === "0.08s"), j(m));
  await cdp.eval(js.closeAll); await sleep(150);
  // 监控抽屉
  await cdp.eval(js.clickMenu("视图", "监控抽屉")); await sleep(350);
  const mon = await cdp.eval(Q.monitor);
  check(`${tag}监控抽屉 transition ${reduced ? "清零" : "含 height+opacity 140ms"}，终态高 150px`, !!mon && mon.h === 150 && (reduced ? mon.td.split(", ").every((x) => x === "0s") : /height/.test(mon.tp) && /opacity/.test(mon.tp) && /0\.14s/.test(mon.td)), j(mon));
  await cdp.eval(js.clickMenu("视图", "监控抽屉")); await sleep(200);
  // toast 进场 / 退场
  const ts = await cdp.eval(Q.toastShow);
  check(`${tag}toast 出现且进场动效 ${reduced ? "清零" : "= toast-in 140ms"}`, !!ts && (reduced ? ts.dur === "0s" || ts.anim === "none" : /toast-in$/.test(ts.anim) && ts.dur === "0.14s"), j(ts));
  if (ts) {
    const dd = await cdp.eval(Q.toastDismiss(ts.id));
    if (reduced) check("【reduce】toast 点 × 立即移除（不等退场那一拍）", !!dd && dd.afterClick.inDom === false, j(dd));
    else check("toast 点 × → 先标 leaving 仍在 DOM，约 160ms 后移除", !!dd && dd.afterClick.leaving && dd.afterClick.inDom && dd.goneLater, j(dd));
  }
  await cdp.eval(Q.clearToasts);
}

async function main() {
  const cdp = await connect(PORT);
  try {
    await cdp.eval(js.closeAll);
    const caps = await cdp.eval(Q.caps);
    check("引擎支持 @starting-style（CSSStartingStyleRule）", caps.startingSupported, caps.chrome);
    check("样式表里确实有 @starting-style 规则", caps.startingRules >= 1, `${caps.startingRules} 条`);
    check("引擎支持 :has()", caps.has);
    // 自定义属性的计算值 Chromium 会按 <time> 规范化（140ms → .14s，80ms 原样），按毫秒比而不是按字面比
    const toMs = (v) => (/^\s*(\d*\.?\d+)\s*ms\s*$/.test(v) ? Number(v.trim().replace("ms", "")) : /^\s*(\d*\.?\d+)\s*s\s*$/.test(v) ? Number(v.trim().replace("s", "")) * 1000 : NaN);
    check("token --fs-motion-base=140ms / --fs-motion-fast=80ms 落到了 :root", toMs(caps.base) === 140 && toMs(caps.fast) === 80, `${caps.base} / ${caps.fast}`);
    // 本机的系统偏好只作记录：Windows「辅助功能 → 视觉效果 → 动画效果」关着时这里就是 true，
    // 两个方向的判据都靠下面的仿真强制，不受机器设置左右。
    console.log(`info  本机默认 prefers-reduced-motion: ${caps.reduced ? "reduce（系统关了动画效果，用户在本机看不到任何动效——这正是兜底该有的行为）" : "no-preference"}`);
    // 正向：强制 no-preference（否则在关了动画效果的机器上正向判据永远量不到动效）
    await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "no-preference" }] });
    await sleep(200);
    const forcedOff = await cdp.eval(`matchMedia("(prefers-reduced-motion: reduce)").matches`);
    check("仿真 no-preference 生效：matchMedia(reduce) = false", forcedOff === false);
    await run(cdp, false);
    // 反向：强制 reduce
    await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "reduce" }] });
    await sleep(200);
    const reduced = await cdp.eval(`matchMedia("(prefers-reduced-motion: reduce)").matches`);
    check("【reduce】仿真生效：matchMedia(prefers-reduced-motion: reduce) = true", reduced === true);
    await run(cdp, true);
  } finally {
    try {
      await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "" }] });
      await cdp.eval(js.closeAll);
    } catch { /* 收尾尽力而为 */ }
    cdp.close();
  }
  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length} 项，${failed.length} 项 FAIL`);
  process.exit(failed.length ? 1 : 0);
}

main().catch((e) => { console.error("motion-audit 失败:", e.message); process.exit(1); });
