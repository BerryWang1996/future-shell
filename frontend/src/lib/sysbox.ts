/**
 * sysbox.ts — 系统面工具箱（M7.1）的前端类型与纯逻辑。
 *
 * 与 Rust 侧 `fs_sshengine::{services, packages}` 的 serde 形状一一对应；解析在 Rust，
 * 这里只有「拿到结果之后界面该说什么」这类判断——它们同样值得单测，因为出口原文点名了
 * 两处「不得混为一谈」：
 *   · 服务：「没有 systemd」≠「有 systemd 但没列出服务」；
 *   · 补丁：「没有包管理器」≠「0 个可更新」。
 * 这两条一旦在 UI 层被压成同一句空文案，后端那两个变体就白分了。
 */

export interface ServiceUnit {
  name: string;
  load: string;
  active: string;
  sub: string;
  description: string;
}

/** 与 Rust `ServiceListResult` 的 `#[serde(tag = "kind")]` 对齐。 */
export type ServiceListResult = { kind: "no_systemd" } | { kind: "units"; units: ServiceUnit[] };

export interface PackageUpdate {
  name: string;
  current: string | null;
  candidate: string;
}
export type PackageManager = "apt" | "dnf" | "zypper" | "apk" | "pacman";
export type PackageScan =
  | { kind: "no_manager" }
  | { kind: "updates"; manager: PackageManager; updates: PackageUpdate[] };

export type ServiceAction = "start" | "stop" | "restart";

/** 动作 → 用户看得懂的动词。 */
export const ACTION_VERB: Record<ServiceAction, string> = {
  start: "启动",
  stop: "停止",
  restart: "重启",
};

/**
 * 动作 → M7.2 的确认类别。
 *
 * 启动**也走确认**但用 `service.restart` 这一档吗？不——启动一个服务不打断任何在用的连接，
 * 它与停止/重启的风险不同级。但类别集合是稳定串（写进用户的设置库），不能为了这一条
 * 临时加档。故：启动不进确认闸（无破坏性），停止用 `service.stop`，重启用 `service.restart`。
 * 返回 null = 这个动作不需要确认。
 */
export function confirmKindFor(action: ServiceAction): "service.stop" | "service.restart" | null {
  if (action === "stop") return "service.stop";
  if (action === "restart") return "service.restart";
  return null;
}

/** 服务的活动状态 → 状态灯语义（与侧栏/标签的灯共用一套形状词汇）。 */
export function serviceLamp(active: string): "ok" | "warn" | "err" | "off" {
  switch (active) {
    case "active":
      return "ok";
    case "activating":
    case "deactivating":
      return "warn";
    case "failed":
      return "err";
    default:
      return "off"; // inactive / unknown
  }
}

/**
 * 服务列表的空态文案。**两种空的说法必须不同**（出口原文点名）。
 * 返回 null = 有内容，不显示空态。
 */
export function serviceEmptyHint(r: ServiceListResult | null, filtered: number): string | null {
  if (r === null) return null; // 还没取到
  if (r.kind === "no_systemd") {
    return "这台机器没有 systemctl（不是 systemd 系统），服务管理不适用——不是「没有服务」。";
  }
  if (r.units.length === 0) return "systemd 在，但没有列出任何服务单元。";
  if (filtered === 0) return "没有匹配当前筛选的服务。";
  return null;
}

/**
 * 补丁盘点的空态文案。同上，「没查」与「查了是最新」必须说两句话——
 * 一台 Alpine 机器显示「0 个可更新」会让用户以为系统是最新的，而真相是我们根本没查。
 */
export function packageEmptyHint(r: PackageScan | null): string | null {
  if (r === null) return null;
  if (r.kind === "no_manager") {
    return "没找到已知的包管理器（apt / dnf / zypper / apk / pacman），补丁盘点不适用——这不代表系统已是最新。";
  }
  if (r.updates.length === 0) return `${r.manager}：没有可升级的包（已是最新）。`;
  return null;
}

/** 各包管理器的升级命令——**只显示给用户自己去终端执行**，本程序不跑。 */
export const UPGRADE_HINT: Record<PackageManager, string> = {
  apt: "sudo apt-get update && sudo apt-get upgrade",
  dnf: "sudo dnf upgrade",
  zypper: "sudo zypper update",
  apk: "sudo apk upgrade",
  pacman: "sudo pacman -Syu",
};

/** 按名字/描述筛选（大小写不敏感）。 */
export function filterUnits(units: ServiceUnit[], q: string): ServiceUnit[] {
  const s = q.trim().toLowerCase();
  if (!s) return units;
  return units.filter(
    (u) => u.name.toLowerCase().includes(s) || u.description.toLowerCase().includes(s),
  );
}
