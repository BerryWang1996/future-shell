/**
 * 监控指标的异常判定（M4b「监控 × AI 联动」）。
 *
 * # 为什么判定在本地、而不是问模型
 *
 * 与危险命令的档位判定同一个原则：**判定是我们的职责，不是模型的**。
 * 把「这些指标正常吗」交给模型，等于让一次网络往返决定界面上要不要报警——
 * 而它会在离线时、限流时、以及模型今天心情不同时给出不同答案。
 * 模型在这个功能里的角色是「已知某台机器负载 12、8 核、内存 96%，**去查什么**」,
 * 那是它擅长的；「12 算不算高」是一道除法。
 *
 * # 阈值是怎么定的
 *
 * 每一个阈值下面都写了依据。没有依据的阈值不写——一个拍出来的数字会让
 * 「异常」这个词失去意义，而失去意义的告警的下一步是被用户忽略。
 *
 * # 采不到 ≠ 正常
 *
 * 所有输入都是 `null` 可能的（非 Linux 上负载/内存/CPU 全空，见 `monitor.rs` 模块头）。
 * 采不到的指标**不产出任何判定**，既不算正常也不算异常。这一条很重要：
 * 把空值当 0 会让一台 macOS 机器永远显示「一切正常」，而那是在替用户下一个
 * 我们根本没有依据的结论。
 */

/** 严重度。两档而非三档——「有点高」和「正常」在操作上没有区别。 */
export type Severity = "warn" | "critical";

export interface Anomaly {
  /** 指标标识，与面板上的 `data-testid` 后缀对应。 */
  metric: "cpu" | "load" | "mem" | "disk";
  severity: Severity;
  /** 一句话说清「什么值、超过了什么」。这句会原样进 AI 提示词。 */
  detail: string;
}

/** 判定所需的输入。字段名与 `MonitorSnapshot` 一致，便于直接传快照。 */
export interface AnomalyInput {
  cpu_percent?: number | null;
  load_1?: number | null;
  cpu_cores?: number | null;
  mem_used_mb?: number | null;
  mem_total_mb?: number | null;
  disk_used?: string | null;
  disk_total?: string | null;
}

/* ── 阈值 ─────────────────────────────────────────────────────────────── */

/**
 * CPU 使用率。
 *
 * 90% 是 warn 而不是 critical：一台正在跑批的机器长时间 95% 是**正常工作**，
 * 而不是故障。单次快照的 CPU 高从来不足以断定有问题——它只值得看一眼。
 * 没有 critical 档：靠一个瞬时百分比区分「忙」与「出事了」是做不到的，
 * 真正的判据是持续时间，而本模块只看单次快照。
 */
export const CPU_WARN = 90;

/**
 * 负载 ÷ 核数。
 *
 * `1.0` 意味着「可运行任务数刚好等于核数」——那是满载而非过载，所以 warn 定在 **2.0**
 * （排队等待的任务和正在跑的一样多），critical 定在 **5.0**（五倍以上积压，
 * 此时交互式响应通常已经明显变慢）。
 *
 * 这两个数对应运维界长期沿用的经验区间（`load < 核数` 健康 / `2×核数` 值得看 /
 * `5×核数` 明显积压）。它们不是精确科学——负载里也算 I/O 等待，
 * 一台被磁盘拖住的机器 CPU 可能是空的。所以 `detail` 里带上核数，
 * 让 AI 那一步能据此去查是 CPU 还是 I/O。
 */
export const LOAD_PER_CORE_WARN = 2.0;
export const LOAD_PER_CORE_CRITICAL = 5.0;

/**
 * 内存与磁盘的已用百分比。
 *
 * 内存 90/97：Linux 会把空闲内存拿去做页缓存，所以「已用高」本身很常见——
 * 但 `free -m` 的 used 列（本模块的来源）已经排除了 buff/cache，
 * 所以这个数高就是真的高。97% 定 critical：那已经接近 OOM killer 开始动手的区间。
 *
 * 磁盘 85/95：磁盘的特点是**没有回头路**——写满之后服务通常直接不可用，
 * 而清理需要人介入且需要时间。所以门槛比内存低，要更早提醒。
 * 95% 定 critical 还有个具体原因：ext4 默认给 root 保留 5%，
 * 到 95% 时普通用户其实已经写不进去了。
 */
export const MEM_WARN = 90;
export const MEM_CRITICAL = 97;
export const DISK_WARN = 85;
export const DISK_CRITICAL = 95;

/**
 * `df -h` 风格的容量串转字节数。
 *
 * `"12G"` / `"1.5T"` / `"512M"` / `"900K"` / `"1024"`（无单位 = 字节）。
 * 认不出返回 `null`——**不猜**。`df` 的输出在不同 coreutils / busybox 上
 * 会有 `12Gi`、`12.0G`、`12G` 等写法，认不出时放弃判定比猜一个数量级安全得多
 * （猜错一档就是 1024 倍，那会让「磁盘快满」和「磁盘空着」互换）。
 *
 * 用 1024 进制：`df -h` 的 G 是 GiB（`df -H` 才是 10 进制）。
 */
export function parseSize(s: string | null | undefined): number | null {
  if (!s) return null;
  const m = /^([0-9]+(?:\.[0-9]+)?)\s*([KMGTPE]i?)?B?$/i.exec(s.trim());
  if (!m) return null;
  const n = Number(m[1]);
  if (!Number.isFinite(n)) return null;
  const unit = (m[2] ?? "").toUpperCase().replace("I", "");
  const pow = { "": 0, K: 1, M: 2, G: 3, T: 4, P: 5, E: 6 }[unit];
  if (pow === undefined) return null;
  return n * 1024 ** pow;
}

/** 按两档阈值给严重度；都没到返回 null。 */
function grade(value: number, warn: number, critical: number): Severity | null {
  if (value >= critical) return "critical";
  if (value >= warn) return "warn";
  return null;
}

/**
 * 找出所有异常指标。
 *
 * 返回顺序固定（cpu → load → mem → disk）而不是按严重度排——面板上指标的位置
 * 是固定的，异常列表跟着严重度重排会让同一个指标在两次刷新之间跳位置。
 * 需要「最严重的是哪个」时用 {@link worstSeverity}。
 */
export function detectAnomalies(s: AnomalyInput): Anomaly[] {
  const out: Anomaly[] = [];

  if (s.cpu_percent != null && s.cpu_percent >= CPU_WARN) {
    out.push({
      metric: "cpu",
      severity: "warn",
      detail: `CPU 使用率 ${s.cpu_percent.toFixed(1)}%（阈值 ${CPU_WARN}%）`,
    });
  }

  // 负载必须有核数才判。没有核数时**不判**——见模块头「采不到 ≠ 正常」。
  if (s.load_1 != null && s.cpu_cores != null && s.cpu_cores > 0) {
    const per = s.load_1 / s.cpu_cores;
    const sev = grade(per, LOAD_PER_CORE_WARN, LOAD_PER_CORE_CRITICAL);
    if (sev) {
      out.push({
        metric: "load",
        severity: sev,
        detail:
          `1 分钟负载 ${s.load_1.toFixed(2)}，${s.cpu_cores} 核 ` +
          `（每核 ${per.toFixed(2)}，阈值 ${LOAD_PER_CORE_WARN}/${LOAD_PER_CORE_CRITICAL}）`,
      });
    }
  }

  if (s.mem_used_mb != null && s.mem_total_mb != null && s.mem_total_mb > 0) {
    const pct = (s.mem_used_mb / s.mem_total_mb) * 100;
    const sev = grade(pct, MEM_WARN, MEM_CRITICAL);
    if (sev) {
      out.push({
        metric: "mem",
        severity: sev,
        detail:
          `内存已用 ${s.mem_used_mb}/${s.mem_total_mb} MB（${pct.toFixed(1)}%，` +
          `阈值 ${MEM_WARN}/${MEM_CRITICAL}%）`,
      });
    }
  }

  const du = parseSize(s.disk_used);
  const dt = parseSize(s.disk_total);
  if (du != null && dt != null && dt > 0) {
    const pct = (du / dt) * 100;
    const sev = grade(pct, DISK_WARN, DISK_CRITICAL);
    if (sev) {
      out.push({
        metric: "disk",
        severity: sev,
        detail:
          `根分区已用 ${s.disk_used}/${s.disk_total}（${pct.toFixed(1)}%，` +
          `阈值 ${DISK_WARN}/${DISK_CRITICAL}%）`,
      });
    }
  }

  return out;
}

/** 最严重的那一档；无异常返回 null。用于给面板一个整体的角标。 */
export function worstSeverity(list: readonly Anomaly[]): Severity | null {
  if (list.some((a) => a.severity === "critical")) return "critical";
  if (list.length > 0) return "warn";
  return null;
}

/**
 * 把异常列表拼成给 AI 的提问。
 *
 * 三个刻意的选择：
 *
 * ① **只描述现象，不提任何命令**。提示词里写「用 top 看看」会把模型锚定在那一条上，
 *    而它本来能根据「负载高但 CPU 空」推出该查 I/O。
 * ② **要求它给一条只读命令**。诊断的第一步永远是「看」，而不是「改」。
 *    这不是靠提示词保证的（模型可以不听）——它给的命令一样要过 `fs_policy` 闸门，
 *    这句话只是让第一次尝试就落在对的方向上，省一次往返。
 * ③ **带上主机名**。同一个人同时开着五台机器时，「哪台」是他最先要确认的。
 */
export function diagnosePrompt(hostname: string, list: readonly Anomaly[]): string {
  const lines = list.map((a) => `- ${a.detail}`).join("\n");
  const host = hostname.trim() || "这台主机";
  return (
    `${host} 的监控指标出现异常：\n${lines}\n\n` +
    `请给出**一条只读命令**来定位原因（不要修改任何东西）。` +
    `如果这几个指标合起来指向某个具体方向，在理由里说明。`
  );
}
