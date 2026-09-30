import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import SerialBaudDialog from "./SerialBaudDialog.svelte";
import SRC from "./SerialBaudDialog.svelte?raw";

const invokeMock = vi.hoisted(() => vi.fn(async (_cmd?: string, _args?: unknown): Promise<unknown> => undefined));
vi.mock("../lib/ipc", () => ({ invoke: invokeMock }));

/**
 * 改波特率（M7.4 出口标准的「改波特率」那一档）。
 *
 * 要害是**不重开端口**：重开会抖一下 DTR，而很多板子的 DTR 接在复位脚上（Arduino/ESP32
 * 就是靠这个自动进下载模式的）——用户只想换个速率看看，板子却重启了。界面必须把这件事说出来，
 * 否则用户不敢在连着的板子上试。
 */
describe("SerialBaudDialog", () => {
  const el = (id: string) => document.querySelector(`[data-testid="${id}"]`) as HTMLElement | null;

  beforeEach(() => {
    cleanup();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd?: string) =>
      cmd === "serial_common_bauds" ? [9600, 115200] : "9600 8N1",
    );
  });

  it("显示端口与当前参数（不知道现在是什么就不知道该不该改）", async () => {
    render(SerialBaudDialog, { open: true, sessionId: "s1", port: "COM3", current: "115200 8N1" });
    await waitFor(() => expect(el("serial-baud")).not.toBeNull());
    expect(el("serial-baud-port")!.textContent).toBe("COM3");
    expect(el("serial-baud-current")!.textContent).toBe("115200 8N1");
  });

  it("建议值来自后端，且是**可输入**的数字框——非标波特率（250000）要能填", async () => {
    render(SerialBaudDialog, { open: true, sessionId: "s1" });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("serial_common_bauds"));
    const input = el("serial-baud-input") as HTMLInputElement;
    expect(input.tagName).toBe("INPUT");
    expect(input.getAttribute("list")).toBe("serial-baud-list");
  });

  it("没填之前「切换」是禁的", async () => {
    render(SerialBaudDialog, { open: true, sessionId: "s1" });
    await waitFor(() => expect(el("serial-baud-ok")).not.toBeNull());
    expect((el("serial-baud-ok") as HTMLButtonElement).disabled).toBe(true);
  });

  it("切换：带会话 id 与数值调后端，回报的摘要交回装配层", async () => {
    const got: string[] = [];
    render(SerialBaudDialog, { open: true, sessionId: "s1", onChanged: (s: string) => got.push(s) });
    await waitFor(() => expect(el("serial-baud-input")).not.toBeNull());
    await fireEvent.input(el("serial-baud-input")!, { target: { value: "9600" } });
    await fireEvent.click(el("serial-baud-ok")!);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("serial_set_baud", { sessionId: "s1", baud: 9600 }));
    await waitFor(() => expect(got).toEqual(["9600 8N1"]));
  });

  it("失败留在框里说清楚", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "serial_common_bauds") return [9600];
      throw new Error("这块硬件不支持 9600");
    });
    const got: string[] = [];
    render(SerialBaudDialog, { open: true, sessionId: "s1", onChanged: (s: string) => got.push(s) });
    await waitFor(() => expect(el("serial-baud-input")).not.toBeNull());
    await fireEvent.input(el("serial-baud-input")!, { target: { value: "9600" } });
    await fireEvent.click(el("serial-baud-ok")!);
    await waitFor(() => expect(el("serial-baud-error")!.textContent).toContain("不支持"));
    expect(got).toEqual([]);
    expect(el("serial-baud")).not.toBeNull();
  });

  it("文案必须说明不重开端口，以及别的参数要去哪儿改", async () => {
    render(SerialBaudDialog, { open: true, sessionId: "s1" });
    await waitFor(() => expect(el("serial-baud")).not.toBeNull());
    const text = el("serial-baud")!.textContent ?? "";
    expect(text).toContain("不重开端口");
    expect(text).toContain("会话属性");
  });

  it("清单是便利不是白名单：源码里不得把波特率做成只能选的下拉", () => {
    expect(SRC).not.toMatch(/<select[^>]*serial-baud/);
  });

  it("关着时不渲染也不发 IPC", () => {
    render(SerialBaudDialog, { open: false, sessionId: "s1" });
    expect(el("serial-baud")).toBeNull();
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
