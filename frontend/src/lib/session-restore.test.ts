import { describe, it, expect } from "vitest";
import { restoreAction } from "./session-restore";
import APP_SOURCE from "../App.svelte?raw";

describe("restoreAction", () => {
  it("默认（开关开着）且有遗留行 → 弹框询问", () => {
    expect(restoreAction(true, 3)).toBe("ask");
  });

  // 缺陷本体：`ui.restoreUnclosed` 此前全仓只有 SettingsDialog 一处写入、零处读取，
  // 用户取消勾选后照样弹框——界面承诺了一个它并不执行的行为。
  it("开关关掉且有遗留行 → 不弹框", () => {
    expect(restoreAction(false, 3)).not.toBe("ask");
  });

  // 且不是「什么都不做」：不问 = 对每一行都裁决为放弃。只跳过弹框会让 unclosed 表
  // 只进不出，日后重新打开开关会迎面撞上一堆陈年会话。
  it("开关关掉且有遗留行 → 顺带弃单", () => {
    expect(restoreAction(false, 3)).toBe("discard");
  });

  it("没有遗留行 → 两种开关下都无事可做（不得空发一次弃单 IPC）", () => {
    expect(restoreAction(true, 0)).toBe("none");
    expect(restoreAction(false, 0)).toBe("none");
  });

  // 上面几条只证明裁决本身对；裁决对而没人调用，用户看到的还是老样子。
  // App.svelte 没有单元测试（onMount 里串着 vault_status、事件订阅等一串副作用，
  // 要在组件层跑起来得搭整套 IPC 桩），所以这里退一步做结构性钉：
  // 开关确实被读取、结果确实交给了 restoreAction、discard 分支确实发弃单 IPC。
  // 这条挡不住语义走样，但挡得住「接线被顺手删掉」——而缺陷本体正是接线压根不存在。
  it("App.svelte 确实读了这个开关，并把裁决交给 restoreAction 执行", () => {
    expect(APP_SOURCE).toContain('settingGet<boolean>("ui.restoreUnclosed", true)');
    expect(APP_SOURCE).toMatch(/restoreAction\(askOnStart,\s*result\.length\)/);
    expect(APP_SOURCE).toMatch(/case "discard":[\s\S]{0,200}?sessions_discard_unclosed/);
  });
});
