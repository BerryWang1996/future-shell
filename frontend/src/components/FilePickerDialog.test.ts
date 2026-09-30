/**
 * FilePickerDialog 组件测试。
 *
 * 这个组件是为了修用户实测报出的三个问题而写的，所以判据直接照着那三条来：
 * **进得去也回得来**、**关得掉**、**能多选**。
 *
 * 「关得掉」尤其要逐条钉：三条退出路径（Esc / 关闭按钮 / 遮罩）任何一条哑火，
 * 用户就被困在模态里——而那正是旧版的症状。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import FilePickerDialog from "./FilePickerDialog.svelte";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("../lib/ipc", () => ({ invoke }));

/** 目录树：根有 docs/ 与两个文件；docs/ 里有一个文件。 */
function mockTree() {
  invoke.mockImplementation((cmd: string, args: any) => {
    if (cmd !== "local_list") return Promise.resolve(undefined);
    const path = args?.path ?? "";
    if (path === "") {
      return Promise.resolve({
        entries: [
          { name: "docs", is_dir: true, size: 0 },
          { name: "b.bin", is_dir: false, size: 200 },
          { name: "a.bin", is_dir: false, size: 100 },
        ],
        truncated: false,
      });
    }
    if (path === "docs") {
      return Promise.resolve({
        entries: [{ name: "inner.txt", is_dir: false, size: 5 }],
        truncated: false,
      });
    }
    return Promise.resolve({ entries: [], truncated: false });
  });
}

beforeEach(() => {
  invoke.mockReset();
  mockTree();
});

const noop = () => {};

async function openPicker(props: Record<string, unknown> = {}) {
  const onConfirm = vi.fn();
  const onCancel = vi.fn();
  render(FilePickerDialog, {
    props: { open: true, onConfirm, onCancel, ...props },
  });
  await waitFor(() => expect(screen.getByTestId("fp-entries")).toBeTruthy());
  return { onConfirm, onCancel };
}

describe("关不掉的那个 bug：三条退出路径都要真的退出", () => {
  it("Esc 触发 onCancel", async () => {
    const { onCancel } = await openPicker();
    await fireEvent.keyDown(screen.getByTestId("file-picker"), { key: "Escape" });
    expect(onCancel).toHaveBeenCalled();
  });

  it("关闭按钮触发 onCancel", async () => {
    const { onCancel } = await openPicker();
    await fireEvent.click(screen.getByTestId("fp-close"));
    expect(onCancel).toHaveBeenCalled();
  });

  it("取消按钮触发 onCancel", async () => {
    const { onCancel } = await openPicker();
    await fireEvent.click(screen.getByTestId("fp-cancel"));
    expect(onCancel).toHaveBeenCalled();
  });

  it("**组件自己不会把 open 置回 true**——它是纯受控的", async () => {
    // 旧版的「关不掉」根因是调用方每次 effect 重跑都把 pickerOpen 置真。
    // 组件这一侧的对应保证是：它只发 onCancel，不含任何自开逻辑。
    // 判据：open=false 时什么都不渲染，且**不发任何 invoke**
    //（自开的实现会在这里去列目录）。
    invoke.mockClear();
    render(FilePickerDialog, { props: { open: false, onConfirm: noop, onCancel: noop } });
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByTestId("file-picker")).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("进得去也回得来", () => {
  it("点目录进入，列表换成该目录的内容", async () => {
    await openPicker();
    await fireEvent.click(screen.getByText("docs"));
    await waitFor(() => expect(screen.getByText("inner.txt")).toBeTruthy());
  });

  it("**上级按钮在根目录禁用、进目录后可用**", async () => {
    await openPicker();
    expect((screen.getByTestId("fp-up") as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(screen.getByText("docs"));
    await waitFor(() =>
      expect((screen.getByTestId("fp-up") as HTMLButtonElement).disabled).toBe(false),
    );
  });

  it("点上级真的回到上一级", async () => {
    await openPicker();
    await fireEvent.click(screen.getByText("docs"));
    await waitFor(() => expect(screen.getByText("inner.txt")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("fp-up"));
    await waitFor(() => expect(screen.getByText("b.bin")).toBeTruthy());
  });

  it("列表首行的 `..` 与上级按钮等价（手在列表上时不必移到工具栏）", async () => {
    await openPicker();
    // 根目录没有 `..`（已经在根了）
    expect(screen.queryByTestId("fp-dotdot")).toBeNull();
    await fireEvent.click(screen.getByText("docs"));
    await waitFor(() => expect(screen.getByTestId("fp-dotdot")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("fp-dotdot"));
    await waitFor(() => expect(screen.getByText("b.bin")).toBeTruthy());
  });

  it("面包屑首段恒为主目录，点它一键回根", async () => {
    await openPicker();
    await fireEvent.click(screen.getByText("docs"));
    await waitFor(() => expect(screen.getByText("inner.txt")).toBeTruthy());
    const crumbs = screen.getByTestId("fp-crumbs");
    const home = [...crumbs.querySelectorAll("button")].find((b) => b.textContent?.includes("主目录"));
    expect(home, "面包屑里没有主目录那一段").toBeTruthy();
    await fireEvent.click(home!);
    await waitFor(() => expect(screen.getByText("b.bin")).toBeTruthy());
  });
});

describe("多选", () => {
  it("Ctrl 加选两个，确认回调收到两条完整路径", async () => {
    const { onConfirm } = await openPicker();
    await fireEvent.click(screen.getByText("a.bin"));
    await fireEvent.click(screen.getByText("b.bin"), { ctrlKey: true });
    await fireEvent.click(screen.getByTestId("fp-confirm"));
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onConfirm.mock.calls[0][0].sort()).toEqual(["a.bin", "b.bin"]);
  });

  it("**确认按显示顺序输出**，不是点击顺序", async () => {
    // 用户看到的顺序就是传输顺序，多文件进度条上的「第 i/N 个」才对得上。
    const { onConfirm } = await openPicker();
    await fireEvent.click(screen.getByText("b.bin"));
    await fireEvent.click(screen.getByText("a.bin"), { ctrlKey: true });
    await fireEvent.click(screen.getByTestId("fp-confirm"));
    // sortEntries：目录在前，文件按名字序 → a.bin 在 b.bin 之前
    expect(onConfirm.mock.calls[0][0]).toEqual(["a.bin", "b.bin"]);
  });

  it("一个都没选时确认按钮禁用", async () => {
    await openPicker();
    expect((screen.getByTestId("fp-confirm") as HTMLButtonElement).disabled).toBe(true);
  });

  it("底部统计显示条数与总字节", async () => {
    await openPicker();
    await fireEvent.click(screen.getByText("a.bin"));
    await fireEvent.click(screen.getByText("b.bin"), { ctrlKey: true });
    const tally = screen.getByTestId("fp-tally").textContent ?? "";
    expect(tally).toContain("2");
    expect(tally).toContain("300"); // 100 + 200 字节
  });

  it("**点目录不参与选择**——那是「进去」不是「选中」", async () => {
    const { onConfirm } = await openPicker();
    await fireEvent.click(screen.getByText("a.bin"));
    // 进 docs 再回来，选择被清空（跨目录多选会拼出错误的路径）
    await fireEvent.click(screen.getByText("docs"));
    await waitFor(() => expect(screen.getByText("inner.txt")).toBeTruthy());
    expect((screen.getByTestId("fp-confirm") as HTMLButtonElement).disabled).toBe(true);
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("进入子目录后确认，路径带上目录前缀", async () => {
    const { onConfirm } = await openPicker();
    await fireEvent.click(screen.getByText("docs"));
    await waitFor(() => expect(screen.getByText("inner.txt")).toBeTruthy());
    await fireEvent.click(screen.getByText("inner.txt"));
    await fireEvent.click(screen.getByTestId("fp-confirm"));
    expect(onConfirm.mock.calls[0][0]).toEqual(["docs/inner.txt"]);
  });

  it("multi=false 时点第二个替换第一个", async () => {
    const { onConfirm } = await openPicker({ multi: false });
    await fireEvent.click(screen.getByText("a.bin"));
    await fireEvent.click(screen.getByText("b.bin"), { ctrlKey: true });
    await fireEvent.click(screen.getByTestId("fp-confirm"));
    expect(onConfirm.mock.calls[0][0]).toEqual(["b.bin"]);
  });
});

describe("目录模式", () => {
  it("确认返回当前目录，且无需选中任何条目", async () => {
    const { onConfirm } = await openPicker({ mode: "directory" });
    expect((screen.getByTestId("fp-confirm") as HTMLButtonElement).disabled).toBe(false);
    await fireEvent.click(screen.getByText("docs"));
    await waitFor(() => expect(screen.getByText("inner.txt")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("fp-confirm"));
    expect(onConfirm.mock.calls[0][0]).toEqual(["docs"]);
  });

  it("目录模式下点文件不选中（文件不是目录）", async () => {
    const { onConfirm } = await openPicker({ mode: "directory" });
    await fireEvent.click(screen.getByText("a.bin"));
    await fireEvent.click(screen.getByTestId("fp-confirm"));
    // 仍返回当前目录（根），而不是那个文件
    expect(onConfirm.mock.calls[0][0]).toEqual([""]);
  });
});

describe("异常态", () => {
  it("列目录失败要显示错误，不能静默空列表", async () => {
    // 静默空列表会被读作「这个目录是空的」。
    invoke.mockImplementation(() => Promise.reject(new Error("权限不足")));
    render(FilePickerDialog, { props: { open: true, onConfirm: noop, onCancel: noop } });
    await waitFor(() => expect(screen.getByTestId("fp-error")).toBeTruthy());
    expect(screen.getByTestId("fp-error").textContent).toContain("权限不足");
  });

  it("截断时明说「未显示的也选不到」", async () => {
    invoke.mockImplementation(() =>
      Promise.resolve({ entries: [{ name: "x", is_dir: false, size: 1 }], truncated: true }),
    );
    render(FilePickerDialog, { props: { open: true, onConfirm: noop, onCancel: noop } });
    await waitFor(() => expect(screen.getByTestId("fp-truncated")).toBeTruthy());
  });

  it("空目录显示提示而不是一片空白", async () => {
    invoke.mockImplementation(() => Promise.resolve({ entries: [], truncated: false }));
    render(FilePickerDialog, { props: { open: true, onConfirm: noop, onCancel: noop } });
    await waitFor(() => expect(screen.getByTestId("fp-empty")).toBeTruthy());
  });
});
