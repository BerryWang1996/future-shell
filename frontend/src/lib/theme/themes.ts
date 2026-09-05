/**
 * 应用主题元数据（UI 规格 §3.1）：{ id, name, dark, tokens }。
 * 令牌值与 tokens.css 逐条一致；渲染以 tokens.css 为唯一源（dataset.theme 选择块），
 * 本文件供主题菜单、明暗判定与 M4b 用户主题加载器复用同一类型。
 */

export const THEME_TOKEN_KEYS = [
  "bg-app", "bg-panel", "bg-elevated", "bg-input",
  "bg-tab-active", "bg-tab-inactive", "bg-hover",
  "fg-primary", "fg-secondary", "fg-disabled",
  "border", "border-strong",
  "accent", "accent-hover", "accent-fg", "accent-dim",
  "ok", "warn", "danger", "info", "warn-dim", "danger-dim",
  "selection", "term-bg",
  "radius", "radius-lg", "bg-tooltip", "fg-tooltip", "shadow", "density",
] as const;

export type ThemeTokenKey = (typeof THEME_TOKEN_KEYS)[number];

export interface AppTheme {
  id: string;
  name: string;
  dark: boolean;
  tokens: Record<ThemeTokenKey, string>;
}

export const BUILTIN_THEMES: AppTheme[] = [
  {
    id: "obsidian", name: "Obsidian", dark: true,
    tokens: {
      "bg-app": "#0b0d11", "bg-panel": "#12151c", "bg-elevated": "#1a1f29", "bg-input": "#0e1117",
      "bg-tab-active": "#0f1115", "bg-tab-inactive": "#161a22", "bg-hover": "#1e2430",
      "fg-primary": "#d7dbe2", "fg-secondary": "#8b93a3", "fg-disabled": "#545b68",
      "border": "#262c38", "border-strong": "#39414f",
      "accent": "#4f8cff", "accent-hover": "#6b9eff", "accent-fg": "#000000", "accent-dim": "rgba(79,140,255,.14)",
      "ok": "#3fd68c", "warn": "#ffd166", "danger": "#ff5f56", "info": "#56d4dd",
      "warn-dim": "rgba(255,209,102,.14)", "danger-dim": "rgba(255,95,86,.14)",
      "selection": "#2b3a55", "term-bg": "#0f1115",
      "radius": "4px", "radius-lg": "8px", "bg-tooltip": "#1a1f29", "fg-tooltip": "#d7dbe2", "shadow": "0 8px 28px rgba(0,0,0,.5)", "density": "1.2",
    },
  },
  {
    id: "daylight", name: "Daylight", dark: false,
    tokens: {
      "bg-app": "#eef0f4", "bg-panel": "#ffffff", "bg-elevated": "#ffffff", "bg-input": "#ffffff",
      "bg-tab-active": "#ffffff", "bg-tab-inactive": "#e9ebef", "bg-hover": "#e4e8ef",
      "fg-primary": "#1d2433", "fg-secondary": "#5a6474", "fg-disabled": "#a2aab8",
      "border": "#d6dae2", "border-strong": "#bcc2cd",
      "accent": "#2563eb", "accent-hover": "#3b82f6", "accent-fg": "#ffffff", "accent-dim": "rgba(37,99,235,.10)",
      "ok": "#16a34a", "warn": "#b45309", "danger": "#dc2626", "info": "#0891b2",
      "warn-dim": "rgba(217,119,6,.10)", "danger-dim": "rgba(220,38,38,.08)",
      "selection": "#bfdbfe", "term-bg": "#10141c",
      "radius": "4px", "radius-lg": "8px", "bg-tooltip": "#ffffff", "fg-tooltip": "#1d2433", "shadow": "0 8px 28px rgba(15,23,42,.18)", "density": "1.2",
    },
  },
  {
    id: "slate", name: "Slate Blue", dark: true,
    tokens: {
      "bg-app": "#0c1220", "bg-panel": "#131a2b", "bg-elevated": "#1a2338", "bg-input": "#0e1526",
      "bg-tab-active": "#101828", "bg-tab-inactive": "#172034", "bg-hover": "#212c46",
      "fg-primary": "#dbe4f5", "fg-secondary": "#8494b3", "fg-disabled": "#4e5c78",
      "border": "#243049", "border-strong": "#354463",
      "accent": "#22d3ee", "accent-hover": "#4ddff2", "accent-fg": "#06283a", "accent-dim": "rgba(34,211,238,.12)",
      "ok": "#34d399", "warn": "#fbbf24", "danger": "#fb7185", "info": "#38bdf8",
      "warn-dim": "rgba(251,191,36,.12)", "danger-dim": "rgba(251,113,133,.12)",
      "selection": "#1d3a5f", "term-bg": "#101828",
      "radius": "4px", "radius-lg": "8px", "bg-tooltip": "#1a2338", "fg-tooltip": "#dbe4f5", "shadow": "0 8px 28px rgba(0,0,0,.55)", "density": "1.2",
    },
  },
  {
    id: "hc", name: "High Contrast", dark: true,
    tokens: {
      "bg-app": "#000000", "bg-panel": "#000000", "bg-elevated": "#0a0a0a", "bg-input": "#000000",
      "bg-tab-active": "#000000", "bg-tab-inactive": "#000000", "bg-hover": "#1c1c1c",
      "fg-primary": "#ffffff", "fg-secondary": "#d0d0d0", "fg-disabled": "#8a8a8a",
      "border": "#6e6e6e", "border-strong": "#a8a8a8",
      "accent": "#ffd400", "accent-hover": "#ffe14d", "accent-fg": "#000000", "accent-dim": "rgba(255,212,0,.16)",
      "ok": "#4dff88", "warn": "#ffd400", "danger": "#ff6b61", "info": "#61d5ff",
      "warn-dim": "rgba(255,212,0,.16)", "danger-dim": "rgba(255,107,97,.16)",
      "selection": "#3a3a00", "term-bg": "#000000",
      "radius": "2px", "radius-lg": "8px", "bg-tooltip": "#0a0a0a", "fg-tooltip": "#ffffff", "shadow": "0 8px 28px rgba(0,0,0,.9)", "density": "1.2",
    },
  },
];

export function getTheme(id: string): AppTheme | undefined {
  return BUILTIN_THEMES.find((t) => t.id === id);
}
