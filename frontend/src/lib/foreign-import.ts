/**
 * 第三方配置导入的前端类型与纯函数（M4b）。
 *
 * 与 Rust 侧 `app/src/commands/import_cmd.rs` 的 `ForeignPreview` / `ForeignImportOutcome`
 * 一一对应。字段名用 snake_case 是因为 Rust 那边没加 `rename_all` —— 跨语言契约里
 * **名字对上**比名字好看重要，两边各写各的风格才是 bug 的来源。
 */

/** 认出来的格式。与 Rust `ForeignKind` 的 kebab-case 序列化逐字对应。 */
export type ForeignKind = "xshell-session" | "xshell-colors" | "finalshell" | "iterm-colors";

export interface Unresolved {
  /** 源文件里的原始键名（含 section）。 */
  key: string;
  /** 为什么没导入。直接给用户看。 */
  why: string;
}

export interface ImportedProfile {
  name: string;
  host: string;
  port: number;
  username: string;
  auth_hint: string | null;
  private_key_path: string | null;
  group_path: string | null;
}

export interface ImportedScheme {
  name: string;
  foreground: string;
  background: string;
  /** ANSI 16 色，索引 0–15。 */
  ansi: string[];
  cursor: string | null;
}

export interface ForeignPreview {
  kind: ForeignKind;
  filename: string;
  profiles: ImportedProfile[];
  schemes: ImportedScheme[];
  unresolved: Unresolved[];
}

export interface ForeignImportOutcome {
  profiles_added: number;
  schemes_added: number;
  schemes_skipped: string[];
}

/**
 * 格式的中文名。
 *
 * 显示格式名而不只是文件名，是因为**认错格式是可能的**——一个内容像 INI 的文件
 * 会被认成 Xshell 会话。用户看到「配色文件 xxx.xsh → Xshell 会话」就知道哪里不对，
 * 而只显示文件名的话，他要等到导入完成、连接列表里多出一条空连接才发现。
 */
export const KIND_LABEL: Record<ForeignKind, string> = {
  "xshell-session": "Xshell 会话",
  "xshell-colors": "Xshell 配色",
  finalshell: "FinalShell 连接",
  "iterm-colors": "iTerm2 配色",
};

/** 一个文件里有什么，一句话。 */
export function summarize(p: ForeignPreview): string {
  const parts: string[] = [];
  if (p.profiles.length > 0) {
    // 一条连接就把地址写出来——用户核对的是「这是不是我那台机器」，不是「有没有一条」
    parts.push(
      p.profiles.length === 1
        ? `连接 ${p.profiles[0].username}@${p.profiles[0].host}:${p.profiles[0].port}`
        : `连接 ${p.profiles.length} 条`,
    );
  }
  if (p.schemes.length > 0) {
    parts.push(
      p.schemes.length === 1 ? `配色「${p.schemes[0].name}」` : `配色 ${p.schemes.length} 套`,
    );
  }
  // 两者都空是可能的：一个语法正确但内容为空的文件。说「没有可导入的内容」，
  // 而不是显示一个空字符串让这一行看起来像渲染坏了。
  return parts.length > 0 ? parts.join("、") : "没有可导入的内容";
}
