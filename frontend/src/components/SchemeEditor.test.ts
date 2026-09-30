import { describe, it, expect, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/svelte";
import SchemeEditor from "./SchemeEditor.svelte";
import type { StoredCustomScheme } from "../lib/term-schemes";

const mk = (over: Partial<StoredCustomScheme> = {}): StoredCustomScheme => ({
  name: "MyDark",
  foreground: "#cccccc",
  background: "#1e1e2e",
  ansi: Array.from({ length: 16 }, (_, i) => `#0000${i.toString(16)}${i.toString(16)}`),
  cursor: "#f5e0dc",
  ...over,
});

afterEach(() => cleanup());

describe("配色编辑器：新建", () => {
  it("从一套中性深色起步，16 个色位都在", () => {
    render(SchemeEditor, { props: { open: true, initial: null } });
    expect((screen.getByTestId("scheme-name") as HTMLInputElement).value).toBe("");
    for (let i = 0; i < 16; i++) {
      expect(screen.getByTestId(`scheme-ansi-${i}`), `缺 ansi ${i}`).toBeTruthy();
    }
    // 少一个色位就会让某个色号取到 undefined，后端也会拒（ansi 必须恰好 16 项）
    expect(screen.queryByTestId("scheme-ansi-16")).toBeNull();
  });

  it("名字为空时存不了（空名字的配色在列表里认不出来，也没法当 id）", () => {
    render(SchemeEditor, { props: { open: true, initial: null } });
    expect((screen.getByTestId("scheme-save") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId("scheme-name-error").textContent).toContain("不能为空");
  });

  it("填了名字就能存，交出去的是完整一套", async () => {
    let saved: StoredCustomScheme | null = null;
    render(SchemeEditor, {
      props: { open: true, initial: null, onSave: (s: StoredCustomScheme) => (saved = s) },
    });
    await fireEvent.input(screen.getByTestId("scheme-name"), { target: { value: " 我的配色 " } });
    await waitFor(() =>
      expect((screen.getByTestId("scheme-save") as HTMLButtonElement).disabled).toBe(false),
    );
    await fireEvent.click(screen.getByTestId("scheme-save"));
    expect(saved).toBeTruthy();
    // 名字 trim 过：`我的配色` 与 `我的配色 ` 会在列表里显示成两个一样的条目
    expect(saved!.name).toBe("我的配色");
    expect(saved!.ansi.length).toBe(16);
    // 颜色一律小写：存储层只有一种表示形式，后端校验也只认小写
    for (const c of [saved!.foreground, saved!.background, saved!.cursor!, ...saved!.ansi]) {
      expect(c, `${c} 不是小写 #rrggbb`).toMatch(/^#[0-9a-f]{6}$/);
    }
  });
});

describe("配色编辑器：编辑既有的", () => {
  it("载入既有值，而不是又从空白开始", () => {
    render(SchemeEditor, { props: { open: true, initial: mk() } });
    expect((screen.getByTestId("scheme-name") as HTMLInputElement).value).toBe("MyDark");
    expect((screen.getByTestId("scheme-background") as HTMLInputElement).value).toBe("#1e1e2e");
    expect((screen.getByTestId("scheme-ansi-3") as HTMLInputElement).value).toBe("#000033");
  });

  it("重名判错，但改回自己的名字不算重名", async () => {
    render(SchemeEditor, {
      props: { open: true, initial: mk({ name: "A" }), takenNames: ["A", "B"] },
    });
    // 自己的名字：不该报重名
    expect(screen.queryByTestId("scheme-name-error")).toBeNull();

    await fireEvent.input(screen.getByTestId("scheme-name"), { target: { value: "B" } });
    await waitFor(() => expect(screen.getByTestId("scheme-name-error")).toBeTruthy());
    expect(screen.getByTestId("scheme-name-error").textContent).toContain("同名");
    expect((screen.getByTestId("scheme-save") as HTMLButtonElement).disabled).toBe(true);
  });

  it("重名不给存，而不是存下去让后端静默跳过", async () => {
    // 后端那条「同名跳过」是给**导入**用的（一次几十套，逐个问不现实）。
    // 在编辑器里用户明确地在造这一套，静默跳过等于他按了保存却什么也没发生。
    let saved = 0;
    render(SchemeEditor, {
      props: { open: true, initial: null, takenNames: ["A"], onSave: () => saved++ },
    });
    await fireEvent.input(screen.getByTestId("scheme-name"), { target: { value: "A" } });
    await waitFor(() => expect(screen.getByTestId("scheme-name-error")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("scheme-save"));
    expect(saved).toBe(0);
  });

  /**
   * 「编辑 A → 取消 → 新建」不许带着 A 的颜色开始。
   *
   * 不重置的话，用户以为自己在造一套新的，存下来却是 A 的副本——而两套长得一模一样，
   * 他要过很久才会发现。与 ForeignImportDialog 的 reset 是同一类问题。
   */
  it("重开时按 initial 重置，不留上一次的残渣", async () => {
    const { rerender } = render(SchemeEditor, {
      props: { open: true, initial: mk({ name: "A", background: "#111111" }) },
    });
    expect((screen.getByTestId("scheme-background") as HTMLInputElement).value).toBe("#111111");

    await rerender({ open: false, initial: null });
    await rerender({ open: true, initial: null });
    expect((screen.getByTestId("scheme-name") as HTMLInputElement).value).toBe("");
    expect((screen.getByTestId("scheme-background") as HTMLInputElement).value).not.toBe("#111111");
  });
});

describe("配色编辑器：预览", () => {
  /** jsdom 把 style 里的 hex 规范化成 `rgb(r, g, b)`，断言要用同一种表示。 */
  const rgb = (hex: string): string => {
    const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
    return `rgb(${r}, ${g}, ${b})`;
  };

  it("预览用这套配色自己的颜色渲染，而不是只摆一排色块", () => {
    // 只看 16 个色块判断不出一套配色好不好用——前景压在背景上读不读得清，
    // 才是用户真正要看的那件事。
    render(SchemeEditor, { props: { open: true, initial: mk() } });
    const style = screen.getByTestId("scheme-preview").getAttribute("style") ?? "";
    expect(style).toContain(rgb("#1e1e2e")); // 背景
    expect(style).toContain(rgb("#cccccc")); // 前景
    expect(screen.getByTestId("scheme-preview").textContent).toContain("ERROR"); // 真实样例文本
  });

  it("改了背景色，预览当场跟着变", async () => {
    render(SchemeEditor, { props: { open: true, initial: mk() } });
    await fireEvent.input(screen.getByTestId("scheme-background"), {
      target: { value: "#004400" },
    });
    await waitFor(() =>
      expect(screen.getByTestId("scheme-preview").getAttribute("style")).toContain(rgb("#004400")),
    );
  });
});

describe("配色编辑器：关闭", () => {
  it("Esc 与取消都调 onCancel，且不产生保存", async () => {
    for (const how of ["esc", "button"] as const) {
      let cancelled = false;
      let saved = 0;
      render(SchemeEditor, {
        props: {
          open: true,
          initial: mk(),
          onCancel: () => (cancelled = true),
          onSave: () => saved++,
        },
      });
      if (how === "esc") {
        await fireEvent.keyDown(screen.getByTestId("scheme-editor"), { key: "Escape" });
      } else {
        await fireEvent.click(screen.getByTestId("scheme-cancel"));
      }
      expect(cancelled, `${how} 没能取消`).toBe(true);
      expect(saved).toBe(0);
      cleanup();
    }
  });
});
