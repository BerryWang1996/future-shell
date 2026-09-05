/**
 * 系统面工具箱的前端判断（M7.1）。
 *
 * 出口原文点名了两处「不得混为一谈」，它们的最后一道防线在**文案**上——后端把
 * `no_systemd` / `no_manager` 与空列表分成了两个变体，UI 层要是把两者说成同一句话，
 * 那两个变体就白分了。这里逐条钉住。
 */
import { describe, expect, it } from "vitest";
import {
  ACTION_VERB,
  UPGRADE_HINT,
  confirmKindFor,
  filterUnits,
  packageEmptyHint,
  serviceEmptyHint,
  serviceLamp,
  type ServiceUnit,
} from "./sysbox";

const unit = (over: Partial<ServiceUnit> = {}): ServiceUnit => ({
  name: "nginx.service",
  load: "loaded",
  active: "active",
  sub: "running",
  description: "A high performance web server",
  ...over,
});

describe("「没有 systemd」与「没有服务」是两句话", () => {
  it("没有 systemd 时明说本功能不适用，并否掉「没有服务」这个误读", () => {
    const h = serviceEmptyHint({ kind: "no_systemd" }, 0)!;
    expect(h).toContain("systemctl");
    expect(h, "必须点破误读，否则用户盯着空表以为这台机器没跑服务").toContain("不是");
  });

  it("有 systemd 但列表为空 → 另一句话", () => {
    const h = serviceEmptyHint({ kind: "units", units: [] }, 0)!;
    expect(h).toContain("没有列出任何服务单元");
    expect(h).not.toBe(serviceEmptyHint({ kind: "no_systemd" }, 0));
  });

  it("有服务但被筛掉 → 第三句话（不能说成「没有服务」）", () => {
    const h = serviceEmptyHint({ kind: "units", units: [unit()] }, 0)!;
    expect(h).toContain("筛选");
  });

  it("有内容时不出空态；还没取到时也不出（空态不该在加载中闪一下）", () => {
    expect(serviceEmptyHint({ kind: "units", units: [unit()] }, 1)).toBeNull();
    expect(serviceEmptyHint(null, 0)).toBeNull();
  });

  it("三句话互不相同", () => {
    const hints = [
      serviceEmptyHint({ kind: "no_systemd" }, 0),
      serviceEmptyHint({ kind: "units", units: [] }, 0),
      serviceEmptyHint({ kind: "units", units: [unit()] }, 0),
    ];
    expect(new Set(hints).size).toBe(3);
  });
});

describe("「没有包管理器」与「0 个可更新」是两句话", () => {
  it("没有包管理器时明说不适用，并否掉「已是最新」这个误读", () => {
    const h = packageEmptyHint({ kind: "no_manager" })!;
    expect(h).toContain("apt");
    expect(h, "一台 Alpine 机器显示「0 个可更新」会让用户以为系统是最新的").toContain("不代表");
  });

  it("查了确实没有可升级 → 报出是哪个管理器查的", () => {
    const h = packageEmptyHint({ kind: "updates", manager: "apk", updates: [] })!;
    expect(h).toContain("apk");
    expect(h).toContain("已是最新");
    expect(h).not.toBe(packageEmptyHint({ kind: "no_manager" }));
  });

  it("有可升级项时不出空态", () => {
    expect(
      packageEmptyHint({
        kind: "updates",
        manager: "apt",
        updates: [{ name: "nginx", current: "1", candidate: "2" }],
      }),
    ).toBeNull();
  });
});

describe("确认类别：只有会打断服务的动作走确认闸", () => {
  it("停止 / 重启各有自己的类别（用户可以只豁免其中一个）", () => {
    expect(confirmKindFor("stop")).toBe("service.stop");
    expect(confirmKindFor("restart")).toBe("service.restart");
    expect(confirmKindFor("stop")).not.toBe(confirmKindFor("restart"));
  });

  it("启动不走确认：它不打断任何在用的连接，为它弹框只会训练用户闭眼点确认", () => {
    expect(confirmKindFor("start")).toBeNull();
  });

  it("三个动作都有中文动词（确认框标题与 toast 都用它）", () => {
    expect(Object.keys(ACTION_VERB).sort()).toEqual(["restart", "start", "stop"]);
    for (const v of Object.values(ACTION_VERB)) expect(v.length).toBeGreaterThan(0);
  });
});

describe("状态灯：靠形状词汇而不是颜色名", () => {
  it.each([
    ["active", "ok"],
    ["failed", "err"],
    ["activating", "warn"],
    ["deactivating", "warn"],
    ["inactive", "off"],
    ["谁知道这是什么", "off"],
  ])("%s → %s", (active, expected) => {
    expect(serviceLamp(active)).toBe(expected);
  });
});

describe("升级命令只显示不执行", () => {
  it("五种管理器各有一条，且都以 sudo 开头（提示用户这要权限）", () => {
    expect(Object.keys(UPGRADE_HINT).sort()).toEqual(["apk", "apt", "dnf", "pacman", "zypper"]);
    for (const c of Object.values(UPGRADE_HINT)) expect(c.startsWith("sudo ")).toBe(true);
  });
});

describe("筛选", () => {
  const units = [unit(), unit({ name: "ssh.service", description: "OpenBSD Secure Shell server" })];

  it("按名字或描述匹配，大小写不敏感", () => {
    expect(filterUnits(units, "NGINX").map((u) => u.name)).toEqual(["nginx.service"]);
    expect(filterUnits(units, "secure shell").map((u) => u.name)).toEqual(["ssh.service"]);
  });

  it("空筛选返回全部（不是返回空）", () => {
    expect(filterUnits(units, "")).toHaveLength(2);
    expect(filterUnits(units, "   ")).toHaveLength(2);
  });

  it("匹配不到返回空数组", () => {
    expect(filterUnits(units, "postgres")).toEqual([]);
  });
});
