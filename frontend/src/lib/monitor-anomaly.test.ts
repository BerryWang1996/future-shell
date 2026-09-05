import { describe, it, expect } from "vitest";
import {
  CPU_WARN,
  DISK_CRITICAL,
  DISK_WARN,
  LOAD_PER_CORE_CRITICAL,
  LOAD_PER_CORE_WARN,
  MEM_CRITICAL,
  MEM_WARN,
  detectAnomalies,
  diagnosePrompt,
  parseSize,
  worstSeverity,
  type AnomalyInput,
} from "./monitor-anomaly";

/** 一台各项都健康的机器。逐条用例只改自己关心的那一项。 */
const healthy: AnomalyInput = {
  cpu_percent: 12.5,
  load_1: 0.8,
  cpu_cores: 8,
  mem_used_mb: 2048,
  mem_total_mb: 16384,
  disk_used: "12G",
  disk_total: "50G",
};

describe("采不到 ≠ 正常", () => {
  it("全空快照（非 Linux）不产出任何判定", () => {
    // macOS/BSD 上负载三项、内存两项、CPU 一项恒空（见 monitor.rs 模块头）。
    // 把空值当 0 会让这台机器**永远**显示「一切正常」——那是在替用户下一个
    // 我们根本没有依据的结论。
    expect(detectAnomalies({})).toEqual([]);
    expect(
      detectAnomalies({
        cpu_percent: null,
        load_1: null,
        cpu_cores: null,
        mem_used_mb: null,
        mem_total_mb: null,
        disk_used: "",
        disk_total: "",
      }),
    ).toEqual([]);
  });

  it("负载有值但核数采不到时不判负载——而不是假设一个核数", () => {
    // load_1 = 40 在 64 核上是空闲、在 1 核上是灾难。猜错方向会让
    // 「一切正常」和「已经过载」互相调换，那比不判更坏。
    const out = detectAnomalies({ ...healthy, load_1: 40, cpu_cores: null });
    expect(out.map((a) => a.metric)).not.toContain("load");
  });

  it("核数为 0 也不判（除零防护）", () => {
    // 后端已把 0 过滤成 None，但前端不依赖那一层——契约的两侧各自成立。
    const out = detectAnomalies({ ...healthy, load_1: 40, cpu_cores: 0 });
    expect(out.map((a) => a.metric)).not.toContain("load");
  });

  it("总量为 0 时不判内存/磁盘", () => {
    expect(
      detectAnomalies({ mem_used_mb: 100, mem_total_mb: 0 }).map((a) => a.metric),
    ).not.toContain("mem");
    expect(
      detectAnomalies({ disk_used: "1G", disk_total: "0" }).map((a) => a.metric),
    ).not.toContain("disk");
  });
});

/**
 * 阈值的**具体值**必须落在有意义的区间里。
 *
 * 这一节是变异测试逼出来的。原先每条边界用例都写成
 * `detectAnomalies({ cpu_percent: CPU_WARN })`——拿常量自己去测常量，
 * 于是把 `CPU_WARN` 从 90 改成 50，21 条断言**全绿**。
 * 那意味着一台正常跑到 60% 的机器会开始报警，而没有任何东西会提示。
 *
 * 所以这里改用**场景**：描述一台具体的机器，断言它该不该报警。
 * 阈值于是被夹在两个真实场景之间，改动它必须先说明为什么那台机器的判定要变。
 */
describe("阈值的具体值（场景夹逼，不拿常量测常量）", () => {
  it("CPU：跑批到 75% 不报警，95% 报警", () => {
    // 下界的理由：一台在跑编译/备份/批处理的机器长时间 70-80% 是**正常工作**。
    // 对它报警等于教用户忽略这个功能。
    expect(detectAnomalies({ ...healthy, cpu_percent: 75 })).toEqual([]);
    // 上界的理由：95% 已经接近饱和，至少值得看一眼。
    expect(detectAnomalies({ ...healthy, cpu_percent: 95 }).map((a) => a.metric)).toEqual(["cpu"]);
  });

  it("内存：一半用掉不报警，98% 报 critical", () => {
    // 50% 是任何机器的常态。`free -m` 的 used 列已排除 buff/cache，
    // 所以这个数就是真实占用——但一半仍然是一半。
    expect(detectAnomalies({ ...healthy, mem_total_mb: 1000, mem_used_mb: 500 })).toEqual([]);
    // 98%：已经进 OOM killer 会动手的区间。
    const out = detectAnomalies({ ...healthy, mem_total_mb: 1000, mem_used_mb: 980 });
    expect(out.map((a) => a.metric)).toEqual(["mem"]);
    expect(out[0].severity).toBe("critical");
  });

  it("磁盘：用一半不报警，88% 报 warn，96% 报 critical", () => {
    const at = (used: string) =>
      detectAnomalies({ ...healthy, disk_total: "100G", disk_used: used });
    expect(at("50G")).toEqual([]);
    // 88%：还能写，但清理需要人介入且需要时间——要在这时候说。
    expect(at("88G")[0]?.severity).toBe("warn");
    // 96%：ext4 默认给 root 保留 5%，此时普通用户其实已经写不进去了。
    expect(at("96G")[0]?.severity).toBe("critical");
  });

  it("负载：8 核跑到 6（每核 0.75）不报警，8 核跑到 48（每核 6）报 critical", () => {
    // 每核 < 1 意味着还有空闲核心，是健康的。
    expect(detectAnomalies({ ...healthy, cpu_cores: 8, load_1: 6 })).toEqual([]);
    // 每核 6：排队的任务是正在跑的五倍以上，交互式响应通常已明显变慢。
    const out = detectAnomalies({ ...healthy, cpu_cores: 8, load_1: 48 });
    expect(out.map((a) => a.metric)).toEqual(["load"]);
    expect(out[0].severity).toBe("critical");
  });
});

describe("阈值边界（含每个阈值的正反两侧）", () => {
  it("健康机器无异常", () => {
    expect(detectAnomalies(healthy)).toEqual([]);
    expect(worstSeverity(detectAnomalies(healthy))).toBeNull();
  });

  it("CPU：阈值上算异常，阈值下不算（>= 而非 >）", () => {
    expect(detectAnomalies({ ...healthy, cpu_percent: CPU_WARN }).map((a) => a.metric)).toEqual(["cpu"]);
    expect(detectAnomalies({ ...healthy, cpu_percent: CPU_WARN - 0.1 })).toEqual([]);
  });

  it("CPU 只有 warn 一档——瞬时百分比区分不了「忙」与「出事了」", () => {
    // 一台正在跑批的机器长时间 95% 是正常工作。若哪天有人给 CPU 加了 critical 档，
    // 这条会红，逼他回答「靠单次快照凭什么断定是故障」。
    const out = detectAnomalies({ ...healthy, cpu_percent: 100 });
    expect(out).toHaveLength(1);
    expect(out[0].severity).toBe("warn");
  });

  it("负载按每核算，warn/critical 两档各自的边界", () => {
    const cores = 8;
    const at = (perCore: number) =>
      detectAnomalies({ ...healthy, cpu_cores: cores, load_1: perCore * cores });

    expect(at(LOAD_PER_CORE_WARN - 0.01)).toEqual([]);
    expect(at(LOAD_PER_CORE_WARN)[0].severity).toBe("warn");
    expect(at(LOAD_PER_CORE_CRITICAL - 0.01)[0].severity).toBe("warn");
    expect(at(LOAD_PER_CORE_CRITICAL)[0].severity).toBe("critical");
  });

  it("同一个负载值在不同核数上得到不同结论——这就是加 cpu_cores 的全部理由", () => {
    const load_1 = 12;
    // 64 核：每核 0.19，空闲
    expect(detectAnomalies({ ...healthy, load_1, cpu_cores: 64 })).toEqual([]);
    // 8 核：每核 1.5，还没到 warn
    expect(detectAnomalies({ ...healthy, load_1, cpu_cores: 8 })).toEqual([]);
    // 2 核：每核 6，critical
    const two = detectAnomalies({ ...healthy, load_1, cpu_cores: 2 });
    expect(two).toHaveLength(1);
    expect(two[0].severity).toBe("critical");
  });

  it("内存两档边界", () => {
    const total = 1000;
    const at = (pct: number) =>
      detectAnomalies({ ...healthy, mem_total_mb: total, mem_used_mb: Math.round((pct / 100) * total) });
    expect(at(MEM_WARN - 1)).toEqual([]);
    expect(at(MEM_WARN)[0].severity).toBe("warn");
    expect(at(MEM_CRITICAL)[0].severity).toBe("critical");
  });

  it("磁盘两档边界，且门槛比内存低（写满没有回头路）", () => {
    const at = (pct: number) =>
      detectAnomalies({ ...healthy, disk_total: "100G", disk_used: `${pct}G` });
    expect(at(DISK_WARN - 1)).toEqual([]);
    expect(at(DISK_WARN)[0].severity).toBe("warn");
    expect(at(DISK_CRITICAL)[0].severity).toBe("critical");
    // 这条钉的是「磁盘比内存更早提醒」这个设计判断本身，而不是两个具体数字。
    // 若有人把磁盘阈值调到内存之上，这里会红并要求他说明理由。
    expect(DISK_WARN).toBeLessThan(MEM_WARN);
    expect(DISK_CRITICAL).toBeLessThan(MEM_CRITICAL);
  });
});

describe("parseSize", () => {
  it("认得 df -h 的常见写法（1024 进制）", () => {
    expect(parseSize("1024")).toBe(1024); // 无单位 = 字节
    expect(parseSize("1K")).toBe(1024);
    expect(parseSize("512M")).toBe(512 * 1024 ** 2);
    expect(parseSize("12G")).toBe(12 * 1024 ** 3);
    expect(parseSize("1.5T")).toBe(1.5 * 1024 ** 4);
    // busybox / 不同 coreutils 的写法
    expect(parseSize("12Gi")).toBe(12 * 1024 ** 3);
    expect(parseSize("12GB")).toBe(12 * 1024 ** 3);
    expect(parseSize("12.0G")).toBe(12 * 1024 ** 3);
    expect(parseSize(" 12G ")).toBe(12 * 1024 ** 3);
    expect(parseSize("12g")).toBe(12 * 1024 ** 3);
  });

  it("用 1024 而不是 1000——df -h 的 G 是 GiB", () => {
    // 认错进制不会让任何断言红（除了这一条），但会让百分比偏 7%——
    // 恰好足以让一台 88% 的机器显示成 82%，跨过 85% 那道线。
    expect(parseSize("1G")).toBe(1073741824);
    expect(parseSize("1G")).not.toBe(1000000000);
  });

  it("认不出就返回 null，不猜", () => {
    // 猜错一档就是 1024 倍，那会让「磁盘快满」和「磁盘空着」互换。
    for (const bad of ["", "  ", "abc", "12X", "G12", "-5G", "1.2.3G", "12 G B", null, undefined]) {
      expect(parseSize(bad as string | null), String(bad)).toBeNull();
    }
  });
});

describe("多项异常与整体严重度", () => {
  it("顺序固定为 cpu → load → mem → disk，不按严重度重排", () => {
    // 面板上指标的位置是固定的。跟着严重度重排会让同一个指标
    // 在两次刷新之间跳位置——用户正要点它的时候。
    const out = detectAnomalies({
      cpu_percent: 99,
      load_1: 100,
      cpu_cores: 4, // 每核 25 → critical
      mem_used_mb: 990,
      mem_total_mb: 1000, // 99% → critical
      disk_used: "99G",
      disk_total: "100G", // 99% → critical
    });
    expect(out.map((a) => a.metric)).toEqual(["cpu", "load", "mem", "disk"]);
  });

  it("worstSeverity 取最重的一档", () => {
    expect(worstSeverity([])).toBeNull();
    expect(worstSeverity([{ metric: "cpu", severity: "warn", detail: "" }])).toBe("warn");
    expect(
      worstSeverity([
        { metric: "cpu", severity: "warn", detail: "" },
        { metric: "disk", severity: "critical", detail: "" },
      ]),
    ).toBe("critical");
  });
});

describe("diagnosePrompt", () => {
  const list = detectAnomalies({ ...healthy, load_1: 40, cpu_cores: 4 });

  it("带上主机名与每条异常的原文", () => {
    const p = diagnosePrompt("web-01", list);
    expect(p).toContain("web-01");
    expect(p).toContain(list[0].detail);
    // 核数进了提示词：模型要靠它判断「负载高但 CPU 空」该往 I/O 方向查
    expect(p).toContain("4 核");
  });

  it("主机名为空时不留一个悬空的开头", () => {
    for (const h of ["", "   "]) {
      const p = diagnosePrompt(h, list);
      expect(p.startsWith("这台主机")).toBe(true);
    }
  });

  it("要求只读命令，且不提任何具体命令名", () => {
    const p = diagnosePrompt("web-01", list);
    expect(p).toContain("只读命令");
    // 提示词里写「用 top 看看」会把模型锚定在那一条上，而它本来能从
    // 「负载高、CPU 空」推出该查 I/O。这条断言钉住那个刻意的留白。
    for (const cmd of ["top", "htop", "iostat", "vmstat", "ps ", "df ", "du "]) {
      expect(p.includes(cmd), `提示词里出现了 ${cmd}，会锚定模型的答案`).toBe(false);
    }
  });

  it("空列表也能生成（调用方不该依赖这个，但它不该崩）", () => {
    expect(() => diagnosePrompt("web-01", [])).not.toThrow();
  });
});

describe("判定完全在本地，不涉及任何模型或网络", () => {
  it("本模块不 import 任何 IPC / 网络设施", async () => {
    // 与危险命令的档位判定同一个原则：判定是我们的职责，不是模型的。
    // 把「这些指标正常吗」交给模型，等于让一次网络往返决定界面上要不要报警,
    // 而它会在离线时、限流时给出不同答案。
    // 用 __dirname 而不是 import.meta.url：vite 下后者不是 file: scheme，
    // `new URL(...)` 会抛「The URL must be of scheme file」。仓里其他源码扫查
    // （AiPanel.test.ts 等）都是这个写法。
    const { readFileSync } = await import("node:fs");
    const { join } = await import("node:path");
    const src = readFileSync(join(__dirname, "monitor-anomaly.ts"), "utf8");
    const code = src
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/(^|[^:])\/\/.*$/gm, "$1");
    // 禁**一切** import，而不只是禁 ipc/fetch。这个模块是纯函数：任何 import
    // 都意味着它开始依赖别的东西，而下一次有人想「顺便问一下模型」时，
    // 那一行 import 就是入口。零 import 是个能一眼核实的判据，
    // 「只许 import 这些」则要求每次都重新判断一遍。
    for (const banned of ["import", "invoke", "fetch(", "ai_", "await "]) {
      expect(code.includes(banned), `本模块出现了 ${banned}——判定必须是纯本地同步的`).toBe(false);
    }
    // 正向对照：确实读到了源码（不是空串让上面全绿）
    expect(code.length).toBeGreaterThan(1000);
    expect(code).toContain("export function detectAnomalies");
  });
});
