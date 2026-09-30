import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/svelte";
import type { ForeignPreview, ForeignImportOutcome } from "../lib/foreign-import";

/** 后端调用记录：命令名 + 实参。 */
const calls: { cmd: string; args: Record<string, unknown> }[] = [];
/** 各命令的桩答复，由用例逐个设定。抛出的 Error 用来扮演后端拒绝。 */
let previewReply: (filename: string) => ForeignPreview | Error = () => new Error("未设定");
let commitReply: (p: ForeignPreview) => ForeignImportOutcome | Error = () =>
  ({ profiles_added: 0, schemes_added: 0, schemes_skipped: [] });

vi.mock("../lib/ipc", () => ({
  invoke: async <T>(cmd: string, args: Record<string, unknown>): Promise<T> => {
    calls.push({ cmd, args });
    const r =
      cmd === "foreign_import_preview"
        ? previewReply(args.filename as string)
        : commitReply(args.preview as ForeignPreview);
    if (r instanceof Error) throw r;
    return r as T;
  },
}));

import ForeignImportDialog from "./ForeignImportDialog.svelte";

/* ---------- 文件选择器的桩 ---------- */

/**
 * 组件用 `document.createElement("input")` 造一个隐藏的 file input 再 `.click()`。
 * jsdom 里点它不会弹任何东西，所以这里截住 createElement 拿到那个元素，
 * 由用例直接喂文件、直接触发它的 onchange —— 比 dispatchEvent 更确定：
 * onchange 是个 async 函数，直接调用能 await 到它真正跑完，不必轮询。
 */
let lastInput: HTMLInputElement | null = null;
let restoreCreate: (() => void) | null = null;

function stubFilePicker(): void {
  const real = document.createElement.bind(document);
  const spy = vi
    .spyOn(document, "createElement")
    .mockImplementation(((tag: string, opts?: ElementCreationOptions) => {
      const el = real(tag, opts);
      if (tag === "input") {
        lastInput = el as HTMLInputElement;
        (el as HTMLInputElement).click = () => {}; // 不让 jsdom 走原生打开逻辑
      }
      return el;
    }) as typeof document.createElement);
  restoreCreate = () => spy.mockRestore();
}

/** 把一批文件塞进被截住的 input，然后把它的 onchange 跑完。 */
async function dropFiles(files: File[]): Promise<void> {
  const input = lastInput;
  if (!input) throw new Error("文件选择器没有被创建——组件的取文件路径变了？");
  Object.defineProperty(input, "files", { value: files, configurable: true });
  await (input.onchange as unknown as (e: unknown) => Promise<void>)({ target: input });
}

const file = (name: string, body = "内容", size?: number): File => {
  const f = new File([body], name);
  if (size !== undefined) Object.defineProperty(f, "size", { value: size });
  return f;
};

/** 造预览。 */
function preview(over: Partial<ForeignPreview> = {}): ForeignPreview {
  return {
    kind: "xshell-session",
    filename: "a.xsh",
    profiles: [
      {
        name: "生产 web",
        host: "10.0.0.5",
        port: 2222,
        username: "ops",
        auth_hint: "password",
        private_key_path: null,
        group_path: null,
      },
    ],
    schemes: [],
    unresolved: [],
    ...over,
  };
}

async function open(): Promise<void> {
  render(ForeignImportDialog, { props: { open: true } });
  await fireEvent.click(screen.getByTestId("foreign-import-pick"));
}

beforeEach(() => {
  calls.length = 0;
  lastInput = null;
  previewReply = (fn) => preview({ filename: fn });
  commitReply = () => ({ profiles_added: 1, schemes_added: 0, schemes_skipped: [] });
  stubFilePicker();
});

afterEach(() => {
  restoreCreate?.();
  cleanup();
});

describe("从其他软件导入：选文件之前就要说清代价", () => {
  it("「密码与私钥不会被导入」在没选任何文件时就在页面上", async () => {
    // 这一句不是免责声明，是这个功能的定义。它必须出现在**选文件之前**——
    // 用户需要在导入前就知道自己之后还得把密码重录一遍。
    // 若挪到结果区，他会在导进来十几条连接、某天要连上去救火时才发现。
    render(ForeignImportDialog, { props: { open: true } });
    const el = screen.getByTestId("foreign-import-no-credentials");
    expect(el.textContent).toContain("不会被导入");
    // 此刻确实还没选过任何文件（否则这条会因「其实已经预览过了」而假绿）
    expect(calls).toEqual([]);
    expect(screen.queryByTestId("foreign-import-preview")).toBeNull();
  });

  it("四种格式都列了名字，用户不必猜自己的文件能不能导", async () => {
    render(ForeignImportDialog, { props: { open: true } });
    const t = screen.getByTestId("foreign-import-formats").textContent ?? "";
    for (const s of ["Xshell", ".xsh", ".xcs", "FinalShell", "iTerm2", ".itermcolors"]) {
      expect(t, `格式说明里没提到 ${s}`).toContain(s);
    }
  });
});

describe("预览：先看清楚再落库", () => {
  it("选了文件只预览，一条都不写库", async () => {
    await open();
    await dropFiles([file("a.xsh")]);
    await waitFor(() => expect(screen.getByTestId("foreign-import-preview")).toBeTruthy());
    expect(calls.map((c) => c.cmd)).toEqual(["foreign_import_preview"]);
    expect(calls.some((c) => c.cmd === "foreign_import_commit")).toBe(false);
  });

  it("一次多个文件各预览一次，逐个列出来", async () => {
    previewReply = (fn) => preview({ filename: fn });
    await open();
    await dropFiles([file("a.xsh"), file("b.xcs"), file("c.json")]);
    await waitFor(() => expect(screen.getAllByTestId("foreign-import-item").length).toBe(3));
    expect(calls.filter((c) => c.cmd === "foreign_import_preview").map((c) => c.args.filename)).toEqual(
      ["a.xsh", "b.xcs", "c.json"],
    );
  });

  it("认出来的格式显示中文名——认错格式时用户看得出来", async () => {
    // 后端按内容认格式，一个 .xcs 存成 .xsh 会被认成配色。只显示文件名的话，
    // 用户要等到导入完成、连接列表里多出一条空连接才发现。
    previewReply = () =>
      preview({ kind: "xshell-colors", filename: "骗人的.xsh", profiles: [], schemes: [] });
    await open();
    await dropFiles([file("骗人的.xsh")]);
    await waitFor(() => expect(screen.getByTestId("foreign-import-item")).toBeTruthy());
    expect(screen.getByTestId("foreign-import-item").textContent).toContain("Xshell 配色");
  });

  it("没能读取的文件被列出来，不是静默消失", async () => {
    // 一次选了 5 个文件、只成功 3 个，若失败的那 2 个不出现，用户会以为全成功了。
    previewReply = (fn) =>
      fn === "坏的.bin" ? new Error("认不出这是哪种配置文件") : preview({ filename: fn });
    await open();
    await dropFiles([file("a.xsh"), file("坏的.bin")]);
    await waitFor(() => expect(screen.getByTestId("foreign-import-failures")).toBeTruthy());
    expect(screen.getByTestId("foreign-import-failures").textContent).toContain("坏的.bin");
    // 好的那个仍然进了待导入清单——一个坏文件不该拖垮整批
    expect(screen.getAllByTestId("foreign-import-item").length).toBe(1);
  });

  it("超大文件在读进内存之前就被挡下，不推给后端", async () => {
    await open();
    await dropFiles([file("巨大.xsh", "x", 2 * 1024 * 1024)]);
    await waitFor(() => expect(screen.getByTestId("foreign-import-failures")).toBeTruthy());
    expect(screen.getByTestId("foreign-import-failures").textContent).toContain("1 MiB");
    expect(calls).toEqual([]); // 一次 IPC 都没发
  });

  it("没导入的字段有清单，且数量说在明处", async () => {
    previewReply = () =>
      preview({
        unresolved: [
          { key: "CONNECTION:AUTHENTICATION/Password", why: "私有加密，且凭据不应由导入带入" },
          { key: "SessionInfo/Macro", why: "本程序没有对应功能" },
        ],
      });
    await open();
    await dropFiles([file("a.xsh")]);
    await waitFor(() => expect(screen.getByTestId("foreign-import-unresolved")).toBeTruthy());
    const box = screen.getByTestId("foreign-import-unresolved");
    expect(box.textContent).toContain("2 个字段");
    // 折叠而不是隐藏：内容必须在 DOM 里，用户点开就能逐条看到「为什么没导进来」
    const rows = screen.getAllByTestId("foreign-unresolved-row");
    expect(rows.length).toBe(2);
    expect(rows[0].textContent).toContain("Password");
    expect(rows[0].textContent).toContain("凭据不应由导入带入");
  });

  it("没有未解析字段时不显示那一块（空清单比不显示更让人不安）", async () => {
    await open();
    await dropFiles([file("a.xsh")]);
    await waitFor(() => expect(screen.getByTestId("foreign-import-preview")).toBeTruthy());
    expect(screen.queryByTestId("foreign-import-unresolved")).toBeNull();
  });
});

describe("落库：确认之后才写", () => {
  it("没有待导入内容时「导入」按钮点不动", async () => {
    render(ForeignImportDialog, { props: { open: true } });
    expect((screen.getByTestId("foreign-import-commit") as HTMLButtonElement).disabled).toBe(true);
  });

  it("点「导入」才发 commit，每个预览一次", async () => {
    await open();
    await dropFiles([file("a.xsh"), file("b.xcs")]);
    await waitFor(() => expect(screen.getAllByTestId("foreign-import-item").length).toBe(2));
    calls.length = 0;
    await fireEvent.click(screen.getByTestId("foreign-import-commit"));
    await waitFor(() => expect(screen.getByTestId("foreign-import-outcome")).toBeTruthy());
    expect(calls.map((c) => c.cmd)).toEqual(["foreign_import_commit", "foreign_import_commit"]);
    // 发过去的是**预览过的那个对象**，不是重新解析一遍——用户确认的就是他看到的
    expect((calls[0].args.preview as ForeignPreview).filename).toBe("a.xsh");
    expect((calls[1].args.preview as ForeignPreview).filename).toBe("b.xcs");
  });

  it("多个文件的结果合并成一句，不是弹三次", async () => {
    commitReply = () => ({ profiles_added: 2, schemes_added: 1, schemes_skipped: [] });
    await open();
    await dropFiles([file("a.xsh"), file("b.xcs")]);
    await waitFor(() => expect(screen.getAllByTestId("foreign-import-item").length).toBe(2));
    await fireEvent.click(screen.getByTestId("foreign-import-commit"));
    await waitFor(() => expect(screen.getByTestId("foreign-import-outcome")).toBeTruthy());
    const t = screen.getByTestId("foreign-import-outcome").textContent ?? "";
    expect(t).toContain("4"); // 2 + 2 条连接
    expect(t).toContain("2 套"); // 1 + 1 套配色
  });

  it("同名跳过的配色要说出名字，并说清为什么不覆盖", async () => {
    // 覆盖会无声地毁掉用户调了很久的一套配色；只说「跳过 1 套」则等于让他去猜是哪一套。
    commitReply = () => ({
      profiles_added: 0,
      schemes_added: 0,
      schemes_skipped: ["Dracula", "Nord"],
    });
    await open();
    await dropFiles([file("a.xcs")]);
    await waitFor(() => expect(screen.getByTestId("foreign-import-item")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("foreign-import-commit"));
    await waitFor(() => expect(screen.getByTestId("foreign-import-skipped")).toBeTruthy());
    const t = screen.getByTestId("foreign-import-skipped").textContent ?? "";
    expect(t).toContain("Dracula");
    expect(t).toContain("Nord");
    expect(t).toContain("先删掉旧的"); // 告诉他怎么办，而不只是告诉他发生了什么
  });

  it("落库失败也留在界面上，不是关掉了事", async () => {
    commitReply = () => new Error("设置值超过 8192 字节上限");
    await open();
    await dropFiles([file("a.xcs")]);
    await waitFor(() => expect(screen.getByTestId("foreign-import-item")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("foreign-import-commit"));
    await waitFor(() => expect(screen.getByTestId("foreign-import-failures")).toBeTruthy());
    expect(screen.getByTestId("foreign-import-failures").textContent).toContain("8192");
    expect(screen.queryByTestId("foreign-import-outcome")).toBeNull();
  });

  it("落库成功后通知外层刷新连接列表", async () => {
    // 不通知的话，导进来的连接要等到下次重启才出现在侧栏——用户会以为没导进去，
    // 于是再导一遍，然后得到一堆重复条目。
    const seen: ForeignImportOutcome[] = [];
    render(ForeignImportDialog, {
      props: { open: true, onImported: (o: ForeignImportOutcome) => seen.push(o) },
    });
    await fireEvent.click(screen.getByTestId("foreign-import-pick"));
    await dropFiles([file("a.xsh")]);
    await waitFor(() => expect(screen.getByTestId("foreign-import-item")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("foreign-import-commit"));
    await waitFor(() => expect(seen.length).toBe(1));
    expect(seen[0].profiles_added).toBe(1);
  });
});

describe("关闭时不留残渣", () => {
  it("取消后重开，上一批预览不还在那儿", async () => {
    // 状态留着的话，用户第二次打开会看到上次选的文件仍列在「将导入」里，
    // 一点「导入」就把已经导过的东西再导一遍。
    let isOpen = true;
    const { rerender } = render(ForeignImportDialog, {
      props: { open: true, onClose: () => (isOpen = false) },
    });
    await fireEvent.click(screen.getByTestId("foreign-import-pick"));
    await dropFiles([file("a.xsh")]);
    await waitFor(() => expect(screen.getByTestId("foreign-import-item")).toBeTruthy());

    await fireEvent.click(screen.getByTestId("foreign-import-close"));
    expect(isOpen).toBe(false);
    await rerender({ open: false });
    await rerender({ open: true });
    expect(screen.queryByTestId("foreign-import-item")).toBeNull();
  });

  it("Esc 与点遮罩都能关", async () => {
    for (const how of ["esc", "overlay"] as const) {
      let closed = false;
      render(ForeignImportDialog, { props: { open: true, onClose: () => (closed = true) } });
      const dlg = screen.getByTestId("foreign-import-dialog");
      if (how === "esc") await fireEvent.keyDown(dlg, { key: "Escape" });
      else await fireEvent.click(dlg.parentElement!);
      expect(closed, `${how} 没能关闭对话框`).toBe(true);
      cleanup();
    }
  });
});

/* ------------------------------------------------------------------------------
 * 落库失败的口径（2026-08-31 用户真机报出后补）
 *
 * 现场：「没能读取」区里一行 `（落库） — serde: missing field \`id\` at line 1 column 104`。
 * 三处都错，各一条判据：
 *   ① 文件名是**写死的占位符**「（落库）」——用户不知道是哪个文件出的问题；
 *   ② 报错是 Rust 侧 serde 原文——零信息量，还让人以为是自己的文件坏了
 *      （真实原因是本程序拼落库载荷时漏了 id，与用户文件无关）；
 *   ③ 整个 for 循环包在一个 try 里——第一个文件失败就中断其余文件。
 * ---------------------------------------------------------------------------- */
describe("落库失败：说清是哪个文件、说人话、不连坐", () => {
  it("失败挂在真实文件名上，不再是写死的「（落库）」", async () => {
    commitReply = () => new Error("serde: missing field `id` at line 1 column 104");
    await open();
    await dropFiles([file("生产环境.json")]);
    await fireEvent.click(screen.getByTestId("foreign-import-commit"));

    const box = await waitFor(() => screen.getByTestId("foreign-import-failures"));
    expect(box.textContent).toContain("生产环境.json");
    expect(
      box.textContent,
      "写死的占位符会让用户去找一个不存在的文件",
    ).not.toContain("（落库）");
  });

  it("serde 原文被包成人话，并点明「不是你的文件的问题」", async () => {
    commitReply = () => new Error("serde: missing field `id` at line 1 column 104");
    await open();
    await dropFiles([file("a.json")]);
    await fireEvent.click(screen.getByTestId("foreign-import-commit"));

    const box = await waitFor(() => screen.getByTestId("foreign-import-failures"));
    expect(box.textContent).toContain("不是你的文件的问题");
    // 原文保留：排查需要它。人话化是**加一层解释**，不是把信息藏掉。
    expect(box.textContent).toContain("missing field");
  });

  it("一个文件落库失败，其余文件照样落库（不连坐）", async () => {
    let n = 0;
    commitReply = () => {
      n += 1;
      return n === 1
        ? new Error("boom")
        : { profiles_added: 1, schemes_added: 0, schemes_skipped: [] };
    };
    previewReply = (fn) => preview({ filename: fn });
    await open();
    await dropFiles([file("坏.json"), file("好.json")]);
    await fireEvent.click(screen.getByTestId("foreign-import-commit"));

    await waitFor(() => expect(n).toBe(2)); // 第二个文件真的被尝试了
    const box = screen.getByTestId("foreign-import-failures");
    expect(box.textContent).toContain("坏.json");
  });
});
