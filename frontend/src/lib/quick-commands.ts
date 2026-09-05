/**
 * 快速命令集 / 片段库（M4a，路线图 §M4 + UI 规格 §2.2 ⚡）。
 *
 * 一个存储两副面孔：**按钮条**（QuickBar，一键下发高频命令）与**片段库**
 * （QuickCommandsDialog 的增删改查编辑器，FinalShell 式分类管理）。两者读同一份
 * settings 键 `quick.commands`，后端 settings_cmd.rs 有同形状的白名单闸
 * （canonical 值测试两侧互钉）。
 *
 * 参数占位符（Xshell 快速命令集对标，M4a 出口标准「参数占位符替换单测」）：
 * 语法 `{{名称}}`——刻意不用 `${名称}`（与 shell 变量语法撞车，`echo ${PATH}`
 * 会被误提示填参）也不用 `{名称}`（与 shell 花括号展开撞车）。双花括号在
 * shell 里没有既有语义。
 */

/** 一条快速命令/片段。字段包络与 Rust 侧 quick.commands 规则同契约：
 *  id 1..=64（且全库唯一）、name 1..=64、command 1..=2048、category ≤64 可选。 */
export interface QuickCommand {
  id: string;
  name: string;
  command: string;
  category?: string;
}

/** id 生成：crypto.randomUUID 普及于安全上下文（tauri webview 即是）；
 *  无 crypto 时退回时间戳+随机数（防 jsdom 老环境测试崩）。 */
export function newQuickCommandId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  return `qc-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
}

/** 从 settings JSON 文本解析片段库。烂值（库被手改/历史残留）不抛、返回空库：
 *  快速命令是便利功能，坏一行就全炸不可用比没有更糟；空库用户可自行重建。
 *  写侧的形状闸在后端（settings_set 白名单），这里只兜读侧。 */
export function parseQuickCommands(json: string | null | undefined): QuickCommand[] {
  if (!json) return [];
  try {
    const v = JSON.parse(json);
    if (!Array.isArray(v)) return [];
    return v.filter(
      (x): x is QuickCommand =>
        x !== null &&
        typeof x === "object" &&
        typeof x.id === "string" &&
        typeof x.name === "string" &&
        typeof x.command === "string",
    );
  } catch {
    return [];
  }
}

/** 解析命令中的占位符（S309）：首次出现序、去重。`{{{a}}}` 这类三花括号按
 *  内层 `{{a}}` 计——贪婪匹配从最左花括号对起，三花括号不是合法语法也无
 *  合理解释，按最内层处理与用户直觉一致（他们想写的大概率就是 `{{a}}`）。 */
export function parsePlaceholders(command: string): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const m of command.matchAll(/\{\{([^{}]+)\}\}/g)) {
    const name = m[1].trim();
    if (name && !seen.has(name)) {
      seen.add(name);
      out.push(name);
    }
  }
  return out;
}

/** 用给定值替换占位符（S309）。未提供值的占位符**原样保留**（不静默吞掉——
 *  静默吞会把 `systemctl restart {{svc}}` 变成重启一个字面量 `{{svc}}` 的服务，
 *  那比报错危险）；值做字面量替换，不解释反斜杠等转义（命令体是给 shell 的，
 *  转义语义归 shell）。 */
export function fillPlaceholders(command: string, values: Record<string, string>): string {
  return command.replace(/\{\{([^{}]+)\}\}/g, (whole, name: string) => {
    const key = name.trim();
    return key in values ? values[key] : whole;
  });
}

/** 分类分组（QuickBar 的渲染序）：无分类段在最后、片段按库内序。
 *  Map 保插入序，天然满足「分类按首现序、段内按库序」。 */
export function groupByCategory(items: QuickCommand[]): Array<{ category: string; items: QuickCommand[] }> {
  const groups = new Map<string, QuickCommand[]>();
  const uncategorized: QuickCommand[] = [];
  for (const it of items) {
    const cat = it.category?.trim();
    if (cat) {
      const list = groups.get(cat) ?? [];
      list.push(it);
      groups.set(cat, list);
    } else {
      uncategorized.push(it);
    }
  }
  const out = [...groups.entries()].map(([category, items]) => ({ category, items }));
  if (uncategorized.length > 0) out.push({ category: "", items: uncategorized });
  return out;
}

/* ── AI 生成的片段（M4b「监控 × AI 联动」出口后半：AI 生成片段入库可用）──── */

/**
 * AI 生成的片段一律进这个分类。
 *
 * **不混进用户自己的分类里**：AI 给的命令与自己写的命令可信度不同，
 * 而片段库是「一键下发」的东西——用户按下去之前有权知道这一条是谁写的。
 * 一个固定分类让这件事在界面上一眼可见，而不需要额外的标记字段。
 */
export const AI_CATEGORY = "AI";

/** 片段名上限（与 Rust 侧 `quick.commands` 校验同源）。 */
export const QC_NAME_MAX = 64;
/** 命令体上限（同上）。 */
export const QC_COMMAND_MAX = 2048;

/** {@link quickCommandFromAi} 的拒绝原因。用枚举而不是字符串，调用方好照着出文案。 */
export type AiSnippetReject = "empty" | "too-long" | "denied" | "duplicate";

/**
 * 把 AI 给的一条命令做成片段。
 *
 * 拒绝而不是「尽力修正」的四种情形：
 *
 * - `denied`：**策略闸门判定不许执行的命令，不许入库。** 这是本函数存在的主要理由。
 *   片段库是一键下发的地方，而档位是会变的——今天以 read_only 档拒掉的一条
 *   `rm -rf`，存进去之后哪天用户切到 with_confirm，它就成了一个点两下就能跑的按钮。
 *   闸门的裁决必须在**入库这一刻**生效，而不是等到下发那一刻再指望它。
 * - `empty` / `too-long`：越界值不静默截断。截断一条 shell 命令得到的是**另一条**
 *   命令，而它有可能仍然合法、语义完全不同（`rm -rf /tmp/x` 截成 `rm -rf /tmp`）。
 * - `duplicate`：命令体逐字相同的一条已经在库里。不是错误，但也不该重复添加——
 *   片段库里两个一模一样的按钮除了让人犹豫没有别的作用。
 *
 * 名字取命令的前若干字符，不弹对话框问用户。理由是这个动作的价值就在于快；
 * 名字在片段库管理器里随时能改，而一个必填的命名步骤会让人干脆不用这个功能。
 */
export function quickCommandFromAi(
  command: string,
  decision: string,
  existing: readonly QuickCommand[],
): { ok: true; item: QuickCommand } | { ok: false; reason: AiSnippetReject } {
  const cmd = command.trim();
  if (!cmd) return { ok: false, reason: "empty" };
  if (cmd.length > QC_COMMAND_MAX) return { ok: false, reason: "too-long" };
  // 只放行明确允许的三档。**白名单而不是黑名单**：将来多一个裁决档位时，
  // 黑名单会默认放行它，而一个未知的裁决默认应当是「不入库」。
  if (!["auto", "confirm", "strong-confirm"].includes(decision)) {
    return { ok: false, reason: "denied" };
  }
  if (existing.some((e) => e.command === cmd)) return { ok: false, reason: "duplicate" };
  // 名字：单行、压掉连续空白、截到上限。命令里可能有换行（AI 给的多行管道），
  // 而片段名显示在一个按钮上。
  const name = cmd.replace(/\s+/g, " ").slice(0, QC_NAME_MAX);
  return {
    ok: true,
    item: { id: newQuickCommandId(), name, command: cmd, category: AI_CATEGORY },
  };
}
