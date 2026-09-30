import { beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  activateMruNext, activateMruPrev, activateTab, activeTabId, addTab, clearBell, removeTab,
  reorderTab, replaceTabId, resetTabStore, ringBell, setTabError, setTabKeyboardMode, setTabStatus,
  setTransientScheme, tabKeyboardMode, tabs, transientSchemes,
} from "./tabs";
import { keyboardModeDefault } from "./shortcuts"; // F25：验证 addTab 初值跟随默认值 store（运行期调用，ESM 循环安全）

describe("tabs store（bell 角标，UI 规格 §5）", () => {
  beforeEach(resetTabStore);

  it("ringBell 置位 / clearBell 手动清除", () => {
    addTab("s1", "web-01", "p1");
    ringBell("s1");
    expect(get(tabs)[0].bell).toBe(true);
    clearBell("s1");
    expect(get(tabs)[0].bell).toBe(false);
  });

  it("切回标签（activate）自动清除角标", () => {
    addTab("s1", "web-01", "p1");
    addTab("s2", "db-02", "p2");
    ringBell("s1");
    activateTab("s1");
    expect(get(tabs).find((t) => t.id === "s1")?.bell).toBe(false);
    expect(get(activeTabId)).toBe("s1");
  });

  it("removeTab 移除并回退活动标签", () => {
    addTab("s1", "a", "p1");
    addTab("s2", "b", "p2");
    removeTab("s2");
    expect(get(tabs)).toHaveLength(1);
    expect(get(activeTabId)).toBe("s1");
  });
});

describe("tabs store（连接态模型 R5 + MRU/排序原语 i21③）", () => {
  beforeEach(resetTabStore);

  it("addTab 默认建 connecting 占位标签；setTabStatus 四态迁移", () => {
    addTab("s1", "web-01", "p1");
    expect(get(tabs)[0]).toMatchObject({ id: "s1", title: "web-01", profileId: "p1", status: "connecting", host: "" });
    setTabStatus("s1", "connected");
    expect(get(tabs)[0].status).toBe("connected");
    setTabStatus("s1", "disconnected");
    expect(get(tabs)[0].status).toBe("disconnected");
    setTabStatus("s1", "error");
    expect(get(tabs)[0].status).toBe("error");
  });

  it("replaceTabId 占位转正：保持位置/标题/状态/活动性", () => {
    addTab("tmp-1", "web-01", "p1", "connecting");
    addTab("s2", "db-02", "p2");
    activateTab("tmp-1");
    replaceTabId("tmp-1", "sess-42");
    const list = get(tabs);
    expect(list.map((t) => t.id)).toEqual(["sess-42", "s2"]);
    expect(list[0]).toMatchObject({ title: "web-01", profileId: "p1", status: "connecting", bell: false });
    expect(get(activeTabId)).toBe("sess-42");
  });

  it("reorderTab 拖拽排序：移动到目标位置", () => {
    addTab("s1", "a", "p1");
    addTab("s2", "b", "p2");
    addTab("s3", "c", "p3");
    reorderTab("s3", "s1");
    expect(get(tabs).map((t) => t.id)).toEqual(["s3", "s1", "s2"]);
  });

  it("MRU 访问序：activateMruNext/Prev 往返循环（Ctrl+Tab/Ctrl+Shift+Tab，UI 规格 §1.2/§4）", () => {
    addTab("s1", "a", "p1");
    addTab("s2", "b", "p2");
    addTab("s3", "c", "p3"); // 活动 s3，访问序 [s3,s2,s1]
    activateMruNext();
    expect(get(activeTabId)).toBe("s2");
    activateMruNext();
    expect(get(activeTabId)).toBe("s1");
    activateMruPrev();
    expect(get(activeTabId)).toBe("s2");
  });
});

describe("tabs store（per-session 键盘模式 F25 + 错误文案 F29①，UI 规格 §2.12）", () => {
  beforeEach(() => {
    resetTabStore();
    keyboardModeDefault.set("remote");
  });

  it("addTab 初始 keyboardMode 取 keyboardModeDefault 当前值", () => {
    keyboardModeDefault.set("local");
    addTab("s1", "web-01", "p1");
    expect(get(tabs)[0].keyboardMode).toBe("local");
    keyboardModeDefault.set("remote");
    addTab("s2", "db-02", "p2");
    expect(tabKeyboardMode("s2")).toBe("remote"); // 后续新建跟随当时默认值
  });

  it("setTabKeyboardMode 仅改本会话；tabKeyboardMode 未知会话缺省 remote", () => {
    addTab("s1", "web-01", "p1");
    addTab("s2", "db-02", "p2");
    setTabKeyboardMode("s1", "local");
    expect(get(tabs).find((t) => t.id === "s1")?.keyboardMode).toBe("local");
    expect(tabKeyboardMode("s2")).toBe("remote");
    expect(tabKeyboardMode("nope")).toBe("remote");
  });

  it("setTabError 置 error 态并写 errorText", () => {
    addTab("s1", "web-01", "p1");
    setTabError("s1", "认证失败");
    expect(get(tabs)[0]).toMatchObject({ status: "error", errorText: "认证失败" });
  });
});

describe("tabs store（会话临时配色 transientSchemes，R11 transient 层 R16 收进 M1）", () => {
  beforeEach(resetTabStore);

  it("setTransientScheme set/get/clear：跟随默认 = null 清除；关闭标签随 removeTab 释放", () => {
    addTab("s1", "web-01", "p1");
    addTab("s2", "db-02", "p2");
    setTransientScheme("s1", "dracula");
    expect(get(transientSchemes).get("s1")).toBe("dracula");
    expect(get(transientSchemes).has("s2")).toBe(false); // 其他会话不受影响
    setTransientScheme("s1", null); // 「跟随默认」清除项
    expect(get(transientSchemes).has("s1")).toBe(false);
    setTransientScheme("s1", "nord");
    removeTab("s1");
    expect(get(transientSchemes).has("s1")).toBe(false); // 关闭标签即释放（Task 25 出口）
  });

  it("S264：replaceTabId 占位转正随迁临时配色——旧键释放、新键可读、关闭仍能释放", () => {
    addTab("tmp-1", "web-01", "p1", "connecting");
    setTransientScheme("tmp-1", "dracula"); // 占位期（connecting）标签右键设配色
    replaceTabId("tmp-1", "sess-42");
    expect(get(transientSchemes).has("tmp-1")).toBe(false); // 不随迁则此键永久滞留
    expect(get(transientSchemes).get("sess-42")).toBe("dracula"); // 不随迁则配色静默还原
    removeTab("sess-42");
    expect(get(transientSchemes).has("sess-42")).toBe(false); // 释放契约（R16）不因转正而断
  });

  it("S264 不越界：无临时配色的标签转正不凭空造键", () => {
    addTab("tmp-2", "db-02", "p2", "connecting");
    replaceTabId("tmp-2", "sess-43");
    expect(get(transientSchemes).size).toBe(0);
  });
});
