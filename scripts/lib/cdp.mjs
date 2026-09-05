/**
 * cdp.mjs — 对着真跑的应用（WebView2 远程调试口）做核查用的最小 CDP 客户端。
 *
 * 零依赖：Node ≥ 22 的全局 WebSocket + fetch。被 scripts/responsive-audit.mjs（布局）与
 * scripts/motion-audit.mjs（动效）共用——两份核查各抄一份客户端，改一处漏一处。
 *
 * 启动应用时带环境变量 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<port>
 * （9333 常落在 Windows 保留端口段里，连不上先查 `netsh interface ipv4 show excludedportrange protocol=tcp`）。
 */
import fs from "node:fs";
import path from "node:path";

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export class Cdp {
  constructor(url) {
    this.ws = new WebSocket(url);
    this.id = 0;
    this.pending = new Map();
    this.ready = new Promise((res, rej) => {
      this.ws.addEventListener("open", () => res());
      this.ws.addEventListener("error", (e) => rej(new Error("CDP websocket 打不开: " + (e.message ?? ""))));
    });
    this.ws.addEventListener("message", (ev) => {
      const m = JSON.parse(typeof ev.data === "string" ? ev.data : ev.data.toString());
      const p = m.id && this.pending.get(m.id);
      if (!p) return;
      this.pending.delete(m.id);
      m.error ? p.rej(new Error(`${m.error.message} (${m.error.code})`)) : p.res(m.result);
    });
  }
  send(method, params = {}, timeoutMs = 15000) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      const t = setTimeout(() => { this.pending.delete(id); rej(new Error(`${method} 超时 ${timeoutMs}ms`)); }, timeoutMs);
      this.pending.set(id, { res: (v) => { clearTimeout(t); res(v); }, rej: (e) => { clearTimeout(t); rej(e); } });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  /** 在页面里求值（await Promise、按值返回）。页面脚本抛错 → 这里抛 Error，不静默。 */
  async eval(expression) {
    const r = await this.send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error("页面脚本异常: " + (r.exceptionDetails.exception?.description ?? r.exceptionDetails.text));
    return r.result.value;
  }
  close() { this.ws.close(); }
}

/** 取第一个 page 目标的 websocket 地址。 */
export async function pageWsUrl(port) {
  const res = await fetch(`http://127.0.0.1:${port}/json`);
  const pages = await res.json();
  const page = pages.find((p) => p.type === "page") ?? pages[0];
  if (!page) throw new Error("CDP 上没有页面目标");
  return page.webSocketDebuggerUrl;
}

/** 连上并等就绪。 */
export async function connect(port) {
  const cdp = new Cdp(await pageWsUrl(port));
  await cdp.ready;
  return cdp;
}

/** 截当前视口为 PNG 写到 file；失败返回说明串而不抛（截图是证据，不是判据）。 */
export async function screenshot(cdp, file, relativeTo = null) {
  try {
    const { data } = await cdp.send("Page.captureScreenshot", { format: "png" }, 20000);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, Buffer.from(data, "base64"));
    return relativeTo ? path.relative(relativeTo, file) : file;
  } catch (e) {
    return `截图失败: ${e.message}`;
  }
}
