import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

/**
 * 连接态守卫（审计维度 F：「双击连接 → 三处同现连接中 → 成功三处转绿」整条链断裂）。
 *
 * 实测断在四处，共一个根因——**标签生命周期起始于连接之后，而连接过程的进度事件被当成了状态迁移信号**：
 *
 *  F-a 连接期三处静默：`openSession` 先 `await invoke("session_open")` 再 `addTab`。而
 *      `session_open`（session_cmd.rs:46）要走完 TCP、banner、主机密钥校验（含弹框等用户点确认）、
 *      认证、子系统装配、journal 落库才 resolve。从双击到连上的整个窗口——不可达主机上是一整个 TCP
 *      超时——标签栏、侧栏灯、状态栏三处全空。转正原语 `replaceTabId`（含 S264 临时配色随迁）连同
 *      单测一起写好了，**生产零调用**。
 *  F-b 连上永不转绿：`setTabStatus(…, "connected")` 唯一的生产调用点是 `session:status` 事件桥，
 *      而后端六处 `events.status()` 全部发生在 `connect()` 内部（标签尚不存在），标签建起来之后
 *      只剩重连成功那一条。于是连得好好的会话一直转蓝环。
 *  F-c 桥无条件判 connected：重连期标签在、id 相同，本轮第一行进度（「跳板 1/2：直连 …」
 *      /「尝试认证方法：password」）就把一个**还没连上**的标签点绿；若这轮随后拒连，绿点留在原地。
 *  F-d 侧栏灯四态只有 connecting 可达：`sessionStates` 是与 `tabs` 平行的第二份 `$state`，
 *      唯一写入方是 openSession，三个事件桥只更新 tabs。
 *
 * 这份守卫因此分三段，缺一段就仍有一条能悄悄断开的线：
 *  ① `statusTransition` 的判据本身（进度文案不得迁移，且判的必须是 `state` 字段而不是文案）；
 *  ② `openSession` 的**时序**行为（标签必须在 `session_open` 发出之前就已存在；转正必须是原地转正）；
 *  ③ 跨文件 / 跨语言的闭合：Rust 侧那唯一一条带 `state` 的 emit、前端所有 `session:status` 消费方、
 *     以及 `addTab`/`replaceTabId`/`session_open` 的生产调用方集合。
 *
 * 为什么 ③ 不能省：F-a/F-b 的产物（原语 + 单测 + 规格散文）当时全在，缺的只是那一条线，
 * 而没有任何门禁在看线。这是本仓第四次同款（App.svelte S262 台账、SettingsDialog 提示文字、
 * TerminalPane:76 的「Task 20/22 接线时必须回补」），所以判据一律取**集合闭合**而非「出现过」。
 */

/* ---------- 测试替身 ---------- */

// vi.hoisted：mock 工厂被提升到 import 之前执行，普通 const 在那一刻还在 TDZ（同 TerminalPane.test.ts 口径）。
const H = vi.hoisted(() => ({ invokeMock: vi.fn() }));

// 只替换 invoke：`./ipc` 的其余导出（settingGet 等）被 tabs → shortcuts 链路真身依赖，
// 整份替掉等于把「本模块用了哪些 IPC」这件事从断言里抹掉。
vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return { ...actual, invoke: H.invokeMock };
});

import { asConnectFailure, isPendingSessionId, openSession, resetPendingSeq } from "./open-session";
import { statusTransition } from "./session-status";
import { activeTabId, addTab, removeTab, resetTabStore, setTransientScheme, tabs, transientSchemes } from "./tabs";
import { toast } from "./toast";
import type { Profile } from "./types";

const PROFILE: Profile = {
  id: "p-1",
  name: "prod-db",
  group_path: null,
  host: "10.0.0.9",
  port: 2222,
  username: "root",
};

/**
 * 可控的 session_open：返回的 promise 由测试自己 release，用来观察「拨号在途」这一刻的界面状态。
 * 按调用次序逐个发 `realIds`，返回的函数一次性放行全部在途拨号（并发占位是被测场景之一，
 * 只留单个 resolver 会让先发的那一次永远悬着）。其余命令（session_close）直接放行。
 */
function deferredOpen(...realIds: string[]) {
  const pending: (() => void)[] = [];
  let n = 0;
  H.invokeMock.mockImplementation((cmd: string) =>
    cmd === "session_open"
      ? new Promise<string>((resolve) => { const id = realIds[n++]; pending.push(() => resolve(id)); })
      : Promise.resolve(null),
  );
  return () => { for (const f of pending.splice(0)) f(); };
}

beforeEach(() => {
  H.invokeMock.mockReset();
  resetTabStore();
  resetPendingSeq();
  vi.spyOn(console, "error").mockImplementation(() => {}); // 失败路径本就该打日志，只是不必刷屏
});

/* ============================ ① 迁移判据 ============================ */

describe("statusTransition（session:status 是进度文案通道，不是连接成功信号）", () => {
  /**
   * 下面这些串是 `crates/sshengine/src/connect.rs` 真发的（63/201/216/233/491/542 六处
   * `events.status(...)`）。它们全部发生在 `connect()` 内部，其中「拒绝连接」「尝试认证方法」
   * 恰恰是**没有连上**的时刻——原实现把任意一条判成 connected，重连期第一行就点绿。
   */
  const PROGRESS_LINES = [
    "拒绝连接 10.0.0.9:2222——主机密钥策略为 strict 且无 pin",
    "跳板 1/2：直连 bastion.example.com:22",
    "尝试认证方法：password",
    "publickey 未发起：私钥文件不存在",
  ];

  it.each(PROGRESS_LINES)("进度文案不迁移状态：%s", (message) => {
    expect(statusTransition({ session_id: "s1", message })).toBeNull();
  });

  it("没有 state 字段一律不迁移（含与成功文案逐字相同的那条）", () => {
    // 这一条是本用例的要害：判据必须是结构化的 state，不能是 message.includes("已重连")。
    // 文案是给人看的，随时会改措辞、会做 i18n；拿它当协议字段等于把 UI 文案变成不可改的接口。
    expect(statusTransition({ session_id: "s1", message: "已重连（远端为新 shell，屏幕内容未恢复）" })).toBeNull();
    expect(statusTransition({ session_id: "s1" })).toBeNull();
  });

  it("只有 state=\"connected\" 才迁移", () => {
    expect(statusTransition({ session_id: "s1", state: "connected" })).toBe("connected");
    expect(statusTransition({ session_id: "s1", message: "x", state: "connected" })).toBe("connected");
  });

  it("其它 state 值不迁移（未知取值按不迁移处理，不得回落成 connected）", () => {
    for (const state of ["connecting", "disconnected", "error", "CONNECTED", ""]) {
      expect(statusTransition({ session_id: "s1", state })).toBeNull();
    }
  });
});

/* ============================ ② openSession 时序 ============================ */

describe("openSession（占位标签 → 拨号 → 原地转正）", () => {
  it("session_open 发出的那一刻，标签已经在且为 connecting（连接期不再三处静默）", async () => {
    let tabsAtDial: { id: string; status: string }[] = [];
    H.invokeMock.mockImplementation(async (cmd: string) => {
      // 同步读取：mock 体在 invoke 被调用的当拍执行，故这里看到的就是「拨号发出时」的界面状态。
      if (cmd === "session_open") tabsAtDial = get(tabs).map((t) => ({ id: t.id, status: t.status }));
      return cmd === "session_open" ? "s-real" : null;
    });

    await openSession(PROFILE);

    expect(tabsAtDial).toEqual([{ id: "pending:1", status: "connecting" }]);
  });

  it("占位标签带齐 title / host / profileId，并被激活（标签栏 + 侧栏灯 + 状态栏三处的数据源）", async () => {
    const release = deferredOpen("s-real");
    const p = openSession(PROFILE);

    const t = get(tabs)[0];
    expect(isPendingSessionId(t.id)).toBe(true);
    expect(t.title).toBe("root@10.0.0.9");
    expect(t.host).toBe("10.0.0.9:2222"); // 状态栏 host 段；空串等于状态栏那一格是空的
    expect(t.profileId).toBe("p-1"); // 侧栏灯按 profileId 反查挂灯，错了就是灯挂在别的 Profile 上
    expect(t.status).toBe("connecting");
    expect(get(activeTabId)).toBe(t.id);

    release();
    await p;
  });

  it("拨号成功：原地转正为真实 id 并置 connected，返回该 id", async () => {
    H.invokeMock.mockImplementation(async (cmd: string) => (cmd === "session_open" ? "s-real" : null));

    await expect(openSession(PROFILE)).resolves.toBe("s-real");

    expect(get(tabs).map((t) => t.id)).toEqual(["s-real"]);
    expect(get(tabs)[0].status).toBe("connected");
    expect(get(activeTabId)).toBe("s-real");
    expect(get(tabs)[0].profileId).toBe("p-1"); // 转正不得丢 profileId，否则侧栏灯当场熄
  });

  it("转正保持标签位置与临时配色（即：走 replaceTabId，而不是 removeTab + addTab）", async () => {
    const release = deferredOpen("s-real");
    const p = openSession(PROFILE);
    const pendingId = get(tabs)[0].id;

    // 拨号在途时用户干的两件事，都是界面上现成的入口：
    setTransientScheme(pendingId, "dracula"); // 右键 → 本会话配色（S264：以 sessionId 为键）
    addTab("s-other", "other", "p-2"); // 又开了一个标签 → 占位标签不再是末位

    release();
    await p;

    // remove+add 式的「转正」会让标签跑到末位，且 removeTab 顺手把临时配色释放掉（tabs.ts:74）。
    expect(get(tabs).map((t) => t.id)).toEqual(["s-real", "s-other"]);
    expect(get(transientSchemes).get("s-real")).toBe("dracula");
    expect(get(transientSchemes).has(pendingId)).toBe(false); // 旧键必须迁走，否则永久滞留 Map
  });

  it("拨号失败：占位标签就地转 error 并留下错误文案，不撤标签", async () => {
    const before = get(toast).length;
    H.invokeMock.mockRejectedValue("no route to host");

    await expect(openSession(PROFILE)).resolves.toBeNull();

    const t = get(tabs)[0];
    expect(t.id).toBe("pending:1");
    expect(t.status).toBe("error"); // §5 四态里 error 这一态此前同样不可达
    expect(t.errorText).toContain("no route to host");
    // 撤掉标签就等于让「哪一个连接失败了」只剩一条 8 秒后自己消失的 toast。
    expect(get(toast).slice(before).some((x) => x.level === "error")).toBe(true);
  });

  /* 2026-09-01（路线图 4c）：session_open 的 Err 现在是结构化 ConnectFailure。
   * 两条路都要接：结构化的连 failure 一起落标签（面板据此挂按钮），字符串的只落
   * errorText——上一条用例（字符串 rejection）**必须继续绿**，那是老口径的兼容判据。 */
  it("拨号失败（结构化）：failure 整份落标签，errorText 取 summary 原文，toast 不显示 [object Object]", async () => {
    const before = get(toast).length;
    const failure = {
      summary: 'auth failed; tried: []; server allows: ["publickey"]; notes: ["只收密钥"]',
      category: "auth",
      tried: [],
      remaining: ["publickey"],
      notes: ["只收密钥"],
      host: "10.0.0.9",
      port: 2222,
      username: "root",
      fingerprint: "",
      has_jump: false,
    };
    H.invokeMock.mockRejectedValue(failure);

    await expect(openSession(PROFILE)).resolves.toBeNull();

    const t = get(tabs)[0];
    expect(t.status).toBe("error");
    expect(t.errorText).toBe(failure.summary);
    expect(t.failure?.remaining).toEqual(["publickey"]);
    expect(t.failure?.category).toBe("auth");
    const msgs = get(toast).slice(before).map((x) => x.msg);
    expect(msgs.some((m) => m.includes("server allows"))).toBe(true);
    expect(msgs.some((m) => m.includes("[object Object]")), "对象 rejection 不得被 String() 成 [object Object]").toBe(false);
  });

  it("asConnectFailure：只认「有 string 的 summary 与 category」的对象；字符串/空/形状不对一律 null", () => {
    expect(asConnectFailure("no route")).toBeNull();
    expect(asConnectFailure(null)).toBeNull();
    expect(asConnectFailure({ summary: 1 })).toBeNull();
    expect(asConnectFailure({ summary: "x" })).toBeNull();
    expect(asConnectFailure({ summary: "x", category: "auth" })).not.toBeNull();
  });

  it("拨号在途时用户关掉占位标签：拨号仍会成功，故必须回头把真会话关掉", async () => {
    const release = deferredOpen("s-real");
    const p = openSession(PROFILE);

    removeTab("pending:1"); // App.closeSession 对占位标签正是这么走的（不发 session_close IPC）
    release();

    await expect(p).resolves.toBeNull();
    expect(get(tabs)).toEqual([]); // 不得把一个用户已经关掉的标签凭空转正回来
    // 后端没有「取消拨号」入口：不补这一刀，注册表里就留着一个界面上再无入口的活会话——
    // 连着远端、占着 SFTP/传输子系统、拿着凭据，直到进程退出。
    expect(H.invokeMock).toHaveBeenCalledWith("session_close", { sessionId: "s-real" });
  });

  it("多个占位标签并存时 id 不撞，各自转正各自的真实 id", async () => {
    const release = deferredOpen("s-a", "s-b");
    const p1 = openSession(PROFILE);
    const p2 = openSession({ ...PROFILE, id: "p-2", host: "10.0.0.10" });

    expect(get(tabs).map((t) => t.id)).toEqual(["pending:1", "pending:2"]);

    release();
    await Promise.all([p1, p2]);

    // 占位 id 一旦复用，第二次 replaceTabId 会把两个标签一起改名（tabs.ts:134 按 id 匹配全表）。
    expect(get(tabs).map((t) => t.id)).toEqual(["s-a", "s-b"]);
    expect(get(tabs).every((t) => t.status === "connected")).toBe(true);
  });
});

/* ============================ ③ 跨文件 / 跨语言闭合 ============================ */

// ?raw 取源文本（同 action-wiring.test.ts / settings-wiring.test.ts 既有口径）。
const FE_RAW = import.meta.glob("../**/*.{ts,svelte}", { query: "?raw", import: "default", eager: true }) as Record<string, string>;
const RS_RAW = {
  ...import.meta.glob("../../../app/src/**/*.rs", { query: "?raw", import: "default", eager: true }),
  ...import.meta.glob("../../../crates/*/src/**/*.rs", { query: "?raw", import: "default", eager: true }),
} as Record<string, string>;

/** glob 的 key 相对**导入方**目录，故一律按文件名比对（本仓这些文件名全仓唯一）。 */
const fileName = (p: string): string => p.replace(/^.*\//, "");

/** 剥注释再判：解释接线的注释里必然写着这些名字，扫全文等于让注释给自己作证。 */
function code(src: string): string {
  return src
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/(^|[^:])\/\/.*$/gm, "$1"); // [^:] 让 https:// 不被当成行注释
}

/** 生产前端源码（排除测试自身：本文件里就写着 "session:status"，不排除就会自我满足）。 */
const FE_PROD: [string, string][] = Object.entries(FE_RAW)
  .filter(([p]) => !fileName(p).includes(".test."))
  .map(([p, raw]) => [fileName(p), code(raw)]);

const RS_PROD: [string, string][] = Object.entries(RS_RAW).map(([p, raw]) => [
  fileName(p),
  raw.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:])\/\/.*$/gm, "$1"),
]);

/** 生产代码里调用了 re 的文件名集合（definer 由调用方自行排除）。 */
const callers = (re: RegExp): string[] =>
  FE_PROD.filter(([, src]) => re.test(src)).map(([n]) => n).sort();

describe("跨文件闭合：连接态的每一段线都必须有唯一且具名的承接方", () => {
  it("openSession 是唯一建标签的地方：addTab / replaceTabId 的生产调用方只有 open-session.ts", () => {
    // F-a 的直接判据。把 addTab 挪回 App.svelte 的 await 之后、或把 replaceTabId 重新变成零调用，
    // 都会在这里转红——那两件事此前都发生过，且全套门禁全绿。
    expect(callers(/\baddTab\(/).filter((n) => n !== "tabs.ts")).toEqual(["open-session.ts"]);
    // 4d（2026-09-04）：RdpPane 的重连也是「原地转正」——RDP 重连是**新会话新 id**，
    // 返回值若被丢弃，标签永远指着死掉的旧 id（画布冻着、关标签也关不掉新会话）。
    // 它走的仍是 tabs.ts 的原语，只是多了一个合法调用方。
    expect(callers(/\breplaceTabId\(/).filter((n) => n !== "tabs.ts")).toEqual([
      "RdpPane.svelte",
      "open-session.ts",
    ]);
  });

  it("session_open 的生产调用方只有 open-session.ts（恢复会话等路径不得另起一条无占位标签的线）", () => {
    expect(callers(/invoke[^(]*\(\s*"session_open"/)).toEqual(["open-session.ts"]);
  });

  it("setTabStatus(…, \"connected\") 只允许出现在 open-session.ts", () => {
    // F-b/F-c 的直接判据：事件桥若重新自己判 connected（无论有没有过 statusTransition），这里转红。
    // RdpPane 那一条（4d）不是从事件猜状态——rdp_reconnect 的返回值是后端对连接
    // 成功的**显式应答**，与 openSession 里 session_open 的返回值同构。
    expect(callers(/setTabStatus\([^)]*"connected"/)).toEqual(["RdpPane.svelte", "open-session.ts"]);
  });

  it("所有 session:status 消费方都经 statusTransition 判迁移", () => {
    const consumers = FE_PROD.filter(([, src]) => src.includes('"session:status"'));
    // 集合本身也钉住：新增一个消费方而忘了走纯函数，会先在这一条上转红。
    expect(consumers.map(([n]) => n).sort()).toEqual(["App.svelte", "TerminalPane.svelte"]);
    for (const [name, src] of consumers) {
      expect(src, `${name} 直接读了 session:status 却没走 statusTransition`).toContain("statusTransition(");
    }
  });

  it("侧栏状态灯由 tabs 派生，不存在第二份平行状态", () => {
    const app = FE_PROD.find(([n]) => n === "App.svelte")![1];
    expect(app).toMatch(/const sessionStates = \$derived\(/); // F-d
    expect(app).toMatch(/\$tabs\.map\(/);
    // 平行 $state 的写入方形态；派生量写不了，出现即说明有人把它改回去了。
    expect(app).not.toMatch(/sessionStates\.(set|delete|clear)\(/);
  });

  it("占位标签不挂真终端，且关闭时不发 session_close", () => {
    const app = FE_PROD.find(([n]) => n === "App.svelte")![1];
    // term_input/term_resize/term_ack 对未知 session_id 一律 Err("no session")，而 TerminalPane
    // 里这三处都是 `void invoke(...)`：挂上去就是每建一个占位标签甩一串没人接的 Promise 拒绝，
    // 且用户对着黑框敲的字被静默丢弃。
    expect(app).toContain("isPendingSessionId(tab.id)");
    expect(app).toContain("isPendingSessionId(sessionId)");
  });
});

describe("跨语言闭合：state 字段是前后端之间唯一的连接成功约定", () => {
  /** 一条 emit 语句 = 从 "session:status" 到其后第一个分号（json! 字面量内部无分号）。 */
  function statusEmits(): { file: string; stmt: string }[] {
    const out: { file: string; stmt: string }[] = [];
    for (const [file, src] of RS_PROD) {
      let i = src.indexOf('"session:status"');
      while (i !== -1) {
        const end = src.indexOf(";", i);
        out.push({ file, stmt: src.slice(i, end === -1 ? src.length : end) });
        i = src.indexOf('"session:status"', i + 1);
      }
    }
    return out;
  }

  /**
   * 2026-09-04（M7.4）：发送点从两个变成三个——串口会话重连成功也要把标签点绿
   *（`serial_cmd.rs`）。**扩的是名单，不是判据**：每一个带 `state` 的发送点仍然必须
   * 恰好写 `"connected"`，而通用进度那一条（events.rs）仍然一个 state 都不许带。
   * 名单本身照旧钉死：多出来一个没在这儿列名的发送点 = 有人在别处点绿标签，
   * 而那正是 F-c（连接期任何一条进度文案都能把标签点绿）的形态。
   */
  it("每一个带 state 的 session:status 都在重连成功处，且名单固定", () => {
    const emits = statusEmits();
    expect(emits.map((e) => e.file).sort()).toEqual([
      "events.rs",
      "serial_cmd.rs",
      "session_cmd.rs",
    ]);

    const withState = emits.filter((e) => /"state"\s*:/.test(e.stmt));
    expect(withState.map((e) => e.file).sort()).toEqual(["serial_cmd.rs", "session_cmd.rs"]);
    for (const e of withState) {
      expect(e.stmt, e.file).toMatch(/"state"\s*:\s*"connected"/);
    }
  });

  it("events.rs 的通用 status() 不得带 state（它播的是进度，发生在还没连上的时刻）", () => {
    const generic = statusEmits().find((e) => e.file === "events.rs")!;
    // 给它加上 state 等于让「拒绝连接 …」「尝试认证方法：…」也把标签点绿——正是 F-c 的形态。
    expect(generic.stmt).not.toMatch(/"state"\s*:/);
  });

  it("测试里用作反例的进度文案确实是后端在发的（防止样本变成一厢情愿的虚构）", () => {
    const connect = RS_PROD.find(([n]) => n === "connect.rs")![1];
    expect(connect).toContain("拒绝连接 ");
    expect(connect).toContain("尝试认证方法：");
    // 且这些都走 events.status()，即上一条断言覆盖的那个不带 state 的发送点。
    expect(connect).toMatch(/events\.status\(/);
  });
});
