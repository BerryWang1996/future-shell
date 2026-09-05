/**
 * 终端内传输（ZMODEM rz/sz）的前端状态（M4a）。
 *
 * 纯状态归约，不碰 DOM、不发 invoke——组件只订阅结果。这样「一条 progress 事件
 * 该怎么改变界面」可以离线钉住，而不必起一个真会话。
 */

/** 后端 `zmodem:progress` 的载荷（键集恒定，见 app/src/zmodem_bridge.rs 的 emit_progress）。 */
export interface ZmodemProgress {
  sessionId: string;
  /**
   * - `need-file`：远端在跑 `rz`，等我们选本地文件
   * - `receiving` / `sending`：传输中
   * - `saved`：单个文件已落盘（多文件时后面还会有）
   * - `done` / `failed`：整个会话终结
   */
  phase:
    | "need-file"
    | "receiving"
    | "sending"
    | "saved"
    | "needs-decision"
    | "done"
    | "failed";
  name: string;
  path: string | null;
  done: number;
  total: number | null;
  message: string;
  /** 多文件上传（2026-08-26）：当前是第几个文件（1 起数）。单文件恒 1。 */
  file_index?: number;
  /** 多文件上传：本次传输共几个文件。后端旧版本不发（undefined 按未知处理）。 */
  file_count?: number;
  /** `needs-decision`：建议目录（后端按设置/出厂默认算好的）。 */
  suggestedDir?: string | null;
  /** `needs-decision`：spool 文件路径（信息性，前端不直接用）。 */
  spoolPath?: string | null;
  /** `needs-decision`：true = 目标已存在（冲突框）；false/undefined = 问去向。 */
  conflict?: boolean | null;
}

/** 一期待用户决策的下载（`needs-decision` 阶段的界面态）。 */
export interface DownloadDecision {
  /** 对端声明的文件名（已清洗） */
  name: string;
  /** 建议目录 */
  suggestedDir: string;
  /** true = 冲突（覆盖/保留两者/跳过）；false = first-run / choose-dir */
  conflict: boolean;
  /** first-run / choose-dir / conflict（后端的 reason，经 message 传） */
  reason: string;
}

/** 界面用的传输态。 */
export interface TransferView {
  /** 是否该显示进度条 */
  visible: boolean;
  /** 是否该弹「选择本地文件」 */
  needFile: boolean;
  direction: "receive" | "send" | null;
  name: string;
  done: number;
  total: number | null;
  /** 0..1，总大小未知时为 null（不要伪造一个假进度） */
  ratio: number | null;
  /** 落盘路径（终态时用于「打开所在目录」） */
  path: string | null;
  /** 终态提示文案；非终态为空串 */
  message: string;
  /** 终态成败；非终态为 null */
  ok: boolean | null;
  /** 多文件：当前第几个（1 起数）；单文件或未知为 null */
  fileIndex: number | null;
  /** 多文件：共几个；未知为 null */
  fileCount: number | null;
  /** 待用户决策的下载；非 needs-decision 阶段为 null */
  decision: DownloadDecision | null;
}

export const IDLE: TransferView = {
  visible: false,
  needFile: false,
  direction: null,
  name: "",
  done: 0,
  total: null,
  ratio: null,
  path: null,
  message: "",
  ok: null,
  fileIndex: null,
  fileCount: null,
  decision: null,
};

/**
 * 归约一条 progress 事件。
 *
 * 刻意接受 `prev` 而不是只看当条：`receiving` 的里程碑事件不带文件名（名字只在
 * 落盘时才定下来，见后端 Begin 分支），丢掉 prev 会让进度条上的文件名在传输途中
 * 闪成空白。
 */
export function reduce(prev: TransferView, ev: ZmodemProgress): TransferView {
  switch (ev.phase) {
    case "need-file":
      return { ...IDLE, needFile: true, direction: "send" };
    case "receiving":
    case "sending": {
      const direction = ev.phase === "receiving" ? "receive" : "send";
      // 名字为空 = 里程碑事件，沿用上一次的名字
      const name = ev.name || prev.name;
      const total = ev.total ?? prev.total;
      return {
        visible: true,
        needFile: false,
        direction,
        name,
        done: ev.done,
        total,
        ratio: ratioOf(ev.done, total),
        path: ev.path ?? prev.path,
        message: "",
        ok: null,
        fileIndex: ev.file_index ?? prev.fileIndex,
        fileCount: ev.file_count ?? prev.fileCount,
        decision: null,
      };
    }
    case "saved":
      return {
        ...prev,
        visible: true,
        needFile: false,
        name: ev.name || prev.name,
        done: ev.done,
        total: ev.total ?? prev.total,
        ratio: 1,
        path: ev.path ?? prev.path,
        fileIndex: ev.file_index ?? prev.fileIndex,
        fileCount: ev.file_count ?? prev.fileCount,
      };
    case "needs-decision":
      return {
        ...prev,
        visible: true,
        needFile: false,
        name: ev.name || prev.name,
        done: ev.done,
        total: ev.total ?? prev.total,
        decision: {
          name: ev.name || prev.name,
          suggestedDir: ev.suggestedDir ?? "",
          conflict: ev.conflict ?? false,
          reason: ev.message || (ev.conflict ? "conflict" : "first-run"),
        },
      };
    case "done":
    case "failed":
      return {
        ...prev,
        visible: true,
        needFile: false,
        message: ev.message,
        ok: ev.phase === "done",
        path: ev.path ?? prev.path,
      };
    default:
      // 未知 phase 不改状态：后端加了新 phase 时，界面宁可停在上一态，
      // 也不要把进度条清空（清空看起来像「传输凭空消失了」）。
      return prev;
  }
}

/** 进度比例；总大小未知或非法时返回 null（不伪造假进度）。 */
export function ratioOf(done: number, total: number | null): number | null {
  if (total === null || !Number.isFinite(total) || total <= 0) return null;
  return Math.min(1, Math.max(0, done / total));
}

/** 人可读的字节数。 */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  // 整数字节不带小数（"512 B" 而不是 "512.0 B"）
  return i === 0 ? `${v} ${units[i]}` : `${v.toFixed(1)} ${units[i]}`;
}

/** 进度文案：总大小未知时只报已传字节，不报百分比。 */
export function progressText(v: TransferView): string {
  if (v.ratio === null) return formatBytes(v.done);
  return `${formatBytes(v.done)} / ${formatBytes(v.total ?? 0)}（${Math.round(v.ratio * 100)}%）`;
}
