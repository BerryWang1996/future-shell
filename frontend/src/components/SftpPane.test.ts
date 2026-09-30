import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import SftpPane from "./SftpPane.svelte";
// 组件源码原文（同 menus.test.ts 的既有口径）：本地栏那几条测的是「代码里有没有这一行」，
// 尤其是「**没有**删除按钮」——那是一条关于缺席的断言，行为测试证明不了缺席。
import SFTP_SOURCE from "./SftpPane.svelte?raw";

/**
 * 审计2 #38 的呈现面：后端列表带条目上限（`LIST_ENTRY_CAP`），`truncated = true` 时
 * 前端必须如实挂「列表不完整」横幅——把截断的列表伪装成目录全貌，用户会以为
 * 「文件不存在」而不是「列表没显示全」。两栏（本地/远端）各有一条横幅。
 */
const invokeMock = vi.hoisted(() =>
  // 返回类型显式放宽成 unknown：这个 mock 要同时扮演列表命令（{entries,truncated}）与
  // 属性命令（{path,meta,…}）两种形状。若让 TS 从默认实现推断，它会锁成
  // `{entries: never[]; truncated: boolean}`，后续塞真实条目/属性的
  // mockImplementation 全部类型不符。vitest 不查这个，svelte-check 查——它抓到过。
  vi.fn(async (_cmd?: string, _args?: unknown): Promise<unknown> => ({
    entries: [],
    truncated: false,
  })),
);

/** 豁免集读取（confirm-gate → ipc.settingGet）。默认原样回落 = 什么都没被豁免。 */
const settingGetMock = vi.hoisted(() => vi.fn(async (_key: string, fallback: unknown) => fallback));

// invoke 之外还要带上 settingGet/settingSet/reportFrontendError：它们同在 ipc.ts 里，
// 而 M7.3 的 ScriptRunDialog 经 confirm-gate 读豁免集。漏一个不是「没测到」而是
// 整个模块 mock 缺导出 → 运行期抛错（Svelte $effect 里抛，堆栈指向 batch.js，很难认）。
vi.mock("../lib/ipc", () => ({
  invoke: invokeMock,
  settingGet: settingGetMock,
  settingSet: vi.fn(async () => {}),
  reportFrontendError: vi.fn(),
}));

describe("SftpPane 截断横幅（审计2 #38）", () => {
  beforeEach(() => {
    cleanup();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async () => ({ entries: [], truncated: false }));
  });

  it("truncated=false 时不挂横幅", async () => {
    render(SftpPane, { sessionId: "s1" });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sftp_list", expect.anything()));
    expect(document.querySelectorAll('[role="note"]').length).toBe(0);
  });

  it("两侧 truncated=true 时各挂一条「列表不完整」横幅", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => ({
      entries: [],
      truncated: cmd === "local_list" || cmd === "sftp_list",
    }));
    render(SftpPane, { sessionId: "s1" });
    await waitFor(() => {
      expect(document.querySelectorAll('[role="note"]').length).toBe(2);
    });
    const notes = [...document.querySelectorAll('[role="note"]')];
    for (const n of notes) {
      expect(n.textContent).toContain("条目超过上限");
    }
  });
});

/**
 * 审计2 #18：档案 SFTP 默认目录（`sftp.local_dir`/`sftp.remote_dir`）此前是死配置——
 * 可编辑可保存，面板却恒从本地家目录与远端 `.` 起步。修复后：prop 直达两栏起点；
 * 档案列表异步加载、prop 晚到时仍跟随；用户手动导航一次后以用户为准，不再回拽。
 */
describe("SftpPane 档案默认目录（审计2 #18）", () => {
  beforeEach(() => {
    cleanup();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async () => ({ entries: [], truncated: false }));
  });

  it("挂载即从档案目录起步（本地 + 远端两栏）", async () => {
    render(SftpPane, { sessionId: "s1", localDir: "/cfg/local", remoteDir: "cfg-remote" });
    // 两栏的列表调用在同一条 refresh 里先后 await——断言必须等两通都落地再查
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("local_list", { path: "/cfg/local" });
      expect(invokeMock).toHaveBeenCalledWith("sftp_list", {
        sessionId: "s1",
        path: "cfg-remote",
      });
    });
  });

  it("档案目录晚到时仍会跟随（用户未动过那栏）", async () => {
    const view = render(SftpPane, { sessionId: "s1" });
    // 初始：默认起点（本地空路径 = 用户目录、远端 `.`）
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("local_list", { path: "" }));
    // 档案列表异步到达（启动加载/恢复会话）：prop 更新后两栏都要跟上
    await view.rerender({ sessionId: "s1", localDir: "/late/local", remoteDir: "late-r" });
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("local_list", { path: "/late/local" });
      expect(invokeMock).toHaveBeenCalledWith("sftp_list", { sessionId: "s1", path: "late-r" });
    });
  });

  it("用户手动导航后，晚到的档案目录不得回拽", async () => {
    const view = render(SftpPane, { sessionId: "s1", localDir: "/cfg" });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("local_list", { path: "/cfg" }));
    // 手动导航：点「~」面包屑回用户目录
    await fireEvent.click(document.querySelector('button[title="用户目录"]')!);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("local_list", { path: "" }));
    // 档案目录晚到——用户已动过该栏，不得覆盖
    await view.rerender({ sessionId: "s1", localDir: "/late" });
    await new Promise((r) => setTimeout(r, 20)); // 等 effect 结算：本应无任何回拽
    const localCalls = invokeMock.mock.calls.filter((c) => c[0] === "local_list");
    const last = localCalls[localCalls.length - 1][1] as { path: string };
    expect(last.path).toBe("");
    expect(invokeMock).not.toHaveBeenCalledWith("local_list", { path: "/late" });
  });
});

/**
 * M4a 文件属性对话框 + 新建软链。
 *
 * 钉三件在界面上「看起来正常」却会误导用户的事：
 * ① 属性取的是 lstat（链自身）而非解析后的目标；断链显示「目标不可访问」而不是错误框；
 * ② 未知字段（mode/uid/gid）显示「—」，不编默认值；
 * ③ 软链两参不许搞反，链名不许带路径分隔符（否则等于允许往用户看不见的地方写）。
 */
describe("SftpPane 属性对话框与新建软链（M4a）", () => {
  const LINK = {
    name: "cfg.lnk", is_dir: false, is_symlink: true, size: 12, mtime: 1_787_300_000, perms: "rwxrwxrwx",
  };
  const FILE = {
    name: "data.bin", is_dir: false, is_symlink: false, size: 4096, mtime: 1_787_300_100, perms: "rw-r--r--",
  };

  beforeEach(() => {
    cleanup();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: [LINK, FILE], truncated: false };
      if (cmd === "local_list") return { entries: [FILE], truncated: false };
      return { entries: [], truncated: false };
    });
  });

  /** 渲染并单击远端某行。 */
  async function pickRemote(name: string) {
    render(SftpPane, { sessionId: "s1" });
    const row = await waitFor(() => {
      const li = [...document.querySelectorAll('[aria-label="远程文件列表"] li')].find((x) =>
        x.textContent?.includes(name),
      );
      if (!li) throw new Error("row not rendered");
      return li as HTMLElement;
    });
    await fireEvent.click(row);
  }

  it("软链属性显示链自身元数据 + 指向何处 + 目标属性", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: [LINK, FILE], truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "sftp_stat_entry")
        return {
          path: "./cfg.lnk",
          meta: { file_type: "symlink", size: 12, mtime: 1_787_300_000, mode: 0o777, uid: 1000, gid: 1000 },
          link_target: "../shared/app.conf",
          target_meta: { file_type: "regular", size: 8192, mtime: 1_787_300_500, mode: 0o644, uid: 0, gid: 0 },
        };
      return { entries: [], truncated: false };
    });
    await pickRemote("cfg.lnk");
    await fireEvent.click(document.querySelector('[data-testid="remote-props"]')!);
    await waitFor(() => expect(document.querySelector('[data-testid="props-body"]')).not.toBeNull());
    expect(document.querySelector('[data-testid="props-type"]')!.textContent).toContain("符号链接");
    // 链自身的大小（12），不是目标的 8192 —— 本用例的要害
    expect(document.querySelector('[data-testid="props-size"]')!.textContent).toContain("12");
    expect(document.querySelector('[data-testid="props-link-target"]')!.textContent).toContain(
      "../shared/app.conf",
    );
    expect(document.querySelector('[data-testid="props-target-size"]')!.textContent).toContain("8,192");
    expect(document.querySelector('[data-testid="props-broken"]')).toBeNull();
  });

  it("断链：显示「目标不可访问」而不是错误框，链自身属性照旧呈现", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: [LINK], truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "sftp_stat_entry")
        return {
          path: "./cfg.lnk",
          meta: { file_type: "symlink", size: 12, mtime: 1_787_300_000, mode: null, uid: null, gid: null },
          link_target: "/gone/nowhere",
          target_meta: null,
        };
      return { entries: [], truncated: false };
    });
    await pickRemote("cfg.lnk");
    await fireEvent.click(document.querySelector('[data-testid="remote-props"]')!);
    await waitFor(() => expect(document.querySelector('[data-testid="props-broken"]')).not.toBeNull());
    expect(document.querySelector('[data-testid="props-error"]')).toBeNull();
    // 未知权限/属主显示「—」，不编 0644 / 0:0
    expect(document.querySelector('[data-testid="props-mode"]')!.textContent).toBe("—");
    expect(document.querySelector('[data-testid="props-owner"]')!.textContent).toBe("—");
  });

  it("属性读取失败：错误显示在对话框内（用户点的是这一项）", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: [FILE], truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "sftp_stat_entry") throw new Error("No such file");
      return { entries: [], truncated: false };
    });
    await pickRemote("data.bin");
    await fireEvent.click(document.querySelector('[data-testid="remote-props"]')!);
    await waitFor(() => expect(document.querySelector('[data-testid="props-error"]')).not.toBeNull());
    expect(document.querySelector('[data-testid="props-error"]')!.textContent).toContain("No such file");
  });

  it("本地侧属性走 local_stat_entry（同一呈现结构，不同来源）", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: [], truncated: false };
      if (cmd === "local_list") return { entries: [FILE], truncated: false };
      if (cmd === "local_stat_entry")
        return {
          path: "C:\\Users\\me\\data.bin",
          // Windows 本地：mode/uid/gid 本就没有 → 面板须显示「—」，与远端未回属性同一口径
          meta: { file_type: "regular", size: 4096, mtime: 1_787_300_100, mode: null, uid: null, gid: null },
          link_target: null,
          target_meta: null,
        };
      return { entries: [], truncated: false };
    });
    render(SftpPane, { sessionId: "s1" });
    const row = await waitFor(() => {
      const li = [...document.querySelectorAll('[aria-label="本地文件列表"] li')].find((x) =>
        x.textContent?.includes("data.bin"),
      );
      if (!li) throw new Error("local row not rendered");
      return li as HTMLElement;
    });
    await fireEvent.click(row);
    await fireEvent.click(document.querySelector('[data-testid="local-props"]')!);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("local_stat_entry", { path: expect.any(String) }),
    );
    // 呈现结构与远端同一套：Windows 本地无 mode/uid/gid → 「—」，不编 0644
    await waitFor(() => expect(document.querySelector('[data-testid="props-body"]')).not.toBeNull());
    expect(document.querySelector('[data-testid="props-mode"]')!.textContent).toBe("—");
    expect(document.querySelector('[data-testid="props-type"]')!.textContent).toContain("普通文件");
  });

  it("新建软链下发 (target, linkPath) 两参且链名落在当前远端目录", async () => {
    await pickRemote("data.bin");
    await fireEvent.click(document.querySelector('[data-testid="remote-symlink"]')!);
    const name = document.querySelector('[data-testid="symlink-name"]') as HTMLInputElement;
    const target = document.querySelector('[data-testid="symlink-target"]') as HTMLInputElement;
    // 目标默认填选中项（「给这个文件建个链」是最常见的意图）
    expect(target.value).toBe("data.bin");
    await fireEvent.input(name, { target: { value: "latest" } });
    await fireEvent.click(document.querySelector('[data-testid="symlink-ok"]')!);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("sftp_symlink", {
        sessionId: "s1",
        target: "data.bin",
        linkPath: "./latest",
      }),
    );
  });

  it("链名带路径分隔符 → 就地拦下且创建按钮禁用（不许往看不见的地方写）", async () => {
    await pickRemote("data.bin");
    await fireEvent.click(document.querySelector('[data-testid="remote-symlink"]')!);
    await fireEvent.input(document.querySelector('[data-testid="symlink-name"]')!, {
      target: { value: "../evil" },
    });
    await waitFor(() =>
      expect(document.querySelector('[data-testid="symlink-err"]')!.textContent).toContain("路径分隔符"),
    );
    expect((document.querySelector('[data-testid="symlink-ok"]') as HTMLButtonElement).disabled).toBe(true);
    expect(invokeMock).not.toHaveBeenCalledWith("sftp_symlink", expect.anything());
  });

  it("服务端不支持 symlink：错误原样进 pane 错误条（不替它编「不支持」）", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: [FILE], truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "sftp_symlink") throw new Error("sftp: SSH_FX_OP_UNSUPPORTED");
      return { entries: [], truncated: false };
    });
    await pickRemote("data.bin");
    await fireEvent.click(document.querySelector('[data-testid="remote-symlink"]')!);
    await fireEvent.input(document.querySelector('[data-testid="symlink-name"]')!, {
      target: { value: "l" },
    });
    await fireEvent.click(document.querySelector('[data-testid="symlink-ok"]')!);
    await waitFor(() => expect(document.body.textContent).toContain("SSH_FX_OP_UNSUPPORTED"));
  });

  it("多选时属性入口禁用，且单选时启用（两个方向都钉，否则「恒启用」的变异活着）", async () => {
    render(SftpPane, { sessionId: "s1" });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sftp_list", expect.anything()));
    const rows = [...document.querySelectorAll('[aria-label="远程文件列表"] li')] as HTMLElement[];
    const btn = () => document.querySelector('[data-testid="remote-props"]') as HTMLButtonElement;
    // 零选：禁用
    expect(btn().disabled).toBe(true);
    // 单选：启用
    await fireEvent.click(rows[0]);
    await waitFor(() => expect(btn().disabled).toBe(false));
    // 双选：禁用（属性是单项语义——多选下「这一项」没有定义）
    await fireEvent.click(rows[1], { ctrlKey: true });
    await waitFor(() => expect(btn().disabled).toBe(true));
    // 且即使绕过 disabled 直接点，也不该发出属性查询（多选下没有「这一项」）
    invokeMock.mockClear();
    btn().click();
    await new Promise((r) => setTimeout(r, 10));
    expect(invokeMock).not.toHaveBeenCalledWith("sftp_stat_entry", expect.anything());
  });
});

/**
 * M4a Xftp 补齐四项的面板行为：排除过滤器、同步浏览、拖拽增强（多选+文件夹+取消）、
 * 外部编辑器关联。
 */
describe("SftpPane 排除过滤器（M4a）", () => {
  beforeEach(() => {
    cleanup();
    localStorage.clear();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async () => ({ entries: [], truncated: false }));
  });

  it("过滤器串随两栏的列表请求一起下发（唯一匹配器在后端）", async () => {
    render(SftpPane, { sessionId: "s1" });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sftp_list", expect.anything()));
    // 初始为空 → exclude 传 undefined（让后端走无过滤器快路）
    const first = invokeMock.mock.calls.find((c) => c[0] === "sftp_list")![1] as any;
    expect(first.exclude).toBeUndefined();

    await fireEvent.input(document.querySelector('[data-testid="sftp-exclude"]')!, {
      target: { value: "*.tmp;node_modules/" },
    });
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("sftp_list", {
        sessionId: "s1",
        path: ".",
        exclude: "*.tmp;node_modules/",
      });
      expect(invokeMock).toHaveBeenCalledWith("local_list", {
        path: "",
        exclude: "*.tmp;node_modules/",
      });
    });
  });

  it("过滤器持久化（重挂后仍在，用户不用每次重打）", async () => {
    localStorage.setItem("sftp.exclude", "*.o");
    render(SftpPane, { sessionId: "s1" });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("sftp_list", {
        sessionId: "s1",
        path: ".",
        exclude: "*.o",
      }),
    );
    expect((document.querySelector('[data-testid="sftp-exclude"]') as HTMLInputElement).value).toBe("*.o");
  });
});

describe("SftpPane 同步浏览（M4a）", () => {
  const DIRS = {
    local: [{ name: "shared", is_dir: true, is_symlink: false, size: 0, mtime: 1 }],
    remote: [{ name: "shared", is_dir: true, is_symlink: false, size: 0, mtime: 1 }],
  };

  beforeEach(() => {
    cleanup();
    localStorage.clear();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd?: string) => ({
      entries: cmd === "local_list" ? DIRS.local : cmd === "sftp_list" ? DIRS.remote : [],
      truncated: false,
    }));
  });

  it("开关关闭时：进本地目录，远端不动", async () => {
    render(SftpPane, { sessionId: "s1" });
    const row = await waitFor(() => {
      const li = [...document.querySelectorAll('[aria-label="本地文件列表"] li')][0];
      if (!li) throw new Error("no row");
      return li as HTMLElement;
    });
    invokeMock.mockClear();
    await fireEvent.dblClick(row);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("local_list", { path: "shared", exclude: undefined }));
    // 远端仍在 "."（未被带走）
    const remoteCalls = invokeMock.mock.calls.filter((c) => c[0] === "sftp_list");
    expect(remoteCalls.every((c) => (c[1] as any).path === ".")).toBe(true);
  });

  it("开关打开且两侧有同名目录：另一侧跟随进入", async () => {
    localStorage.setItem("sftp.syncBrowse", "1");
    render(SftpPane, { sessionId: "s1" });
    const row = await waitFor(() => {
      const li = [...document.querySelectorAll('[aria-label="本地文件列表"] li')][0];
      if (!li) throw new Error("no row");
      return li as HTMLElement;
    });
    await fireEvent.dblClick(row);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("sftp_list", {
        sessionId: "s1",
        path: "./shared",
        exclude: undefined,
      }),
    );
  });

  it("另一侧没有同名目录：原地不动 + 就地提示（不是错误，也不导航到不存在的路径）", async () => {
    localStorage.setItem("sftp.syncBrowse", "1");
    invokeMock.mockImplementation(async (cmd?: string) => ({
      entries:
        cmd === "local_list"
          ? [{ name: "only-local", is_dir: true, is_symlink: false, size: 0, mtime: 1 }]
          : [],
      truncated: false,
    }));
    render(SftpPane, { sessionId: "s1" });
    const row = await waitFor(() => {
      const li = [...document.querySelectorAll('[aria-label="本地文件列表"] li')][0];
      if (!li) throw new Error("no row");
      return li as HTMLElement;
    });
    await fireEvent.dblClick(row);
    await waitFor(() =>
      expect(document.querySelector('[data-testid="sftp-sync-note"]')!.textContent).toContain(
        "远端当前目录下没有",
      ),
    );
    // 远端未被导航到 ./only-local
    expect(invokeMock).not.toHaveBeenCalledWith("sftp_list", {
      sessionId: "s1",
      path: "./only-local",
      exclude: undefined,
    });
    // 且不是错误条
    expect(document.querySelector('[data-testid="sftp-error"]')).toBeNull();
  });
});

describe("SftpPane 拖拽增强：多选 + 文件夹 + 入队中取消（M4a）", () => {
  const F1 = { name: "a.bin", is_dir: false, is_symlink: false, size: 1, mtime: 1 };
  const F2 = { name: "b.bin", is_dir: false, is_symlink: false, size: 2, mtime: 2 };
  const D1 = { name: "proj", is_dir: true, is_symlink: false, size: 0, mtime: 3 };

  beforeEach(() => {
    cleanup();
    localStorage.clear();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "local_list") return { entries: [D1, F1, F2], truncated: false };
      if (cmd === "sftp_list") return { entries: [], truncated: false };
      if (cmd === "local_walk")
        return { files: ["src/main.rs", "Cargo.toml"], dirs: ["src"], skipped_links: [] };
      return undefined;
    });
  });

  async function localRows() {
    render(SftpPane, { sessionId: "s1" });
    return await waitFor(() => {
      const rows = [...document.querySelectorAll('[aria-label="本地文件列表"] li')] as HTMLElement[];
      if (rows.length < 3) throw new Error("rows not ready");
      return rows;
    });
  }

  it("选中多项后拖其中一项 = 传整个选中集", async () => {
    const rows = await localRows();
    await fireEvent.click(rows[1]); // a.bin
    await fireEvent.click(rows[2], { ctrlKey: true }); // + b.bin
    invokeMock.mockClear();
    // 拖 a.bin 落到远端栏
    const data = new Map<string, string>([["side", "local"], ["name", "a.bin"]]);
    await fireEvent.drop(document.querySelector('section[aria-label="远程文件"]')!, {
      dataTransfer: { getData: (k: string) => data.get(k) ?? "" },
    });
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("transfer_submit", expect.objectContaining({ local: "a.bin" }));
      expect(invokeMock).toHaveBeenCalledWith("transfer_submit", expect.objectContaining({ local: "b.bin" }));
    });
  });

  it("拖不在选中集里的项 = 只传它自己", async () => {
    const rows = await localRows();
    await fireEvent.click(rows[1]); // 选中 a.bin
    invokeMock.mockClear();
    const data = new Map<string, string>([["side", "local"], ["name", "b.bin"]]);
    await fireEvent.drop(document.querySelector('section[aria-label="远程文件"]')!, {
      dataTransfer: { getData: (k: string) => data.get(k) ?? "" },
    });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("transfer_submit", expect.objectContaining({ local: "b.bin" })),
    );
    expect(invokeMock).not.toHaveBeenCalledWith("transfer_submit", expect.objectContaining({ local: "a.bin" }));
  });

  it("拖文件夹：后端递归枚举 → 先建远端子目录 → 逐文件入队（保结构）", async () => {
    await localRows();
    invokeMock.mockClear();
    const data = new Map<string, string>([["side", "local"], ["name", "proj"]]);
    await fireEvent.drop(document.querySelector('section[aria-label="远程文件"]')!, {
      dataTransfer: { getData: (k: string) => data.get(k) ?? "" },
    });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("local_walk", { path: "proj", exclude: undefined }),
    );
    // 目录先建（含顶层 proj 与子目录 proj/src）
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("sftp_mkdir", { sessionId: "s1", path: "./proj" });
      expect(invokeMock).toHaveBeenCalledWith("sftp_mkdir", { sessionId: "s1", path: "./proj/src" });
    });
    // 文件按相对结构入队
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "transfer_submit",
        expect.objectContaining({ local: "proj/src/main.rs", remote: "./proj/src/main.rs" }),
      );
      expect(invokeMock).toHaveBeenCalledWith(
        "transfer_submit",
        expect.objectContaining({ local: "proj/Cargo.toml", remote: "./proj/Cargo.toml" }),
      );
    });
  });

  it("枚举失败（截断/超限）→ 一件不入队，并说明未入队", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "local_list") return { entries: [D1], truncated: false };
      if (cmd === "sftp_list") return { entries: [], truncated: false };
      if (cmd === "local_walk") throw new Error("待传文件超过 20000 个，拒绝入队");
      return undefined;
    });
    render(SftpPane, { sessionId: "s1" });
    // 等**行渲染出来**而不是等 IPC 发出：localEntries 在 refresh 的两次 await 结算后才赋值，
    // 早一拍拖拽会看不到 is_dir，把目录当文件提交。
    await waitFor(() => {
      if (!document.querySelector('[aria-label="本地文件列表"] li')) throw new Error("row not ready");
    });
    invokeMock.mockClear();
    const data = new Map<string, string>([["side", "local"], ["name", "proj"]]);
    await fireEvent.drop(document.querySelector('section[aria-label="远程文件"]')!, {
      dataTransfer: { getData: (k: string) => data.get(k) ?? "" },
    });
    await waitFor(() => expect(document.body.textContent).toContain("未入队任何作业"));
    expect(invokeMock).not.toHaveBeenCalledWith("transfer_submit", expect.anything());
  });

  it("软链被跳过时如实说出来（静默跳过 = 谎报完整性）", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "local_list") return { entries: [D1], truncated: false };
      if (cmd === "sftp_list") return { entries: [], truncated: false };
      if (cmd === "local_walk")
        return { files: ["real.txt"], dirs: [], skipped_links: ["danger.lnk"] };
      return undefined;
    });
    render(SftpPane, { sessionId: "s1" });
    await waitFor(() => {
      if (!document.querySelector('[aria-label="本地文件列表"] li')) throw new Error("row not ready");
    });
    const data = new Map<string, string>([["side", "local"], ["name", "proj"]]);
    await fireEvent.drop(document.querySelector('section[aria-label="远程文件"]')!, {
      dataTransfer: { getData: (k: string) => data.get(k) ?? "" },
    });
    await waitFor(() =>
      expect(document.querySelector('[data-testid="sftp-sync-note"]')!.textContent).toContain(
        "符号链接",
      ),
    );
    expect(document.querySelector('[data-testid="sftp-sync-note"]')!.textContent).toContain("danger.lnk");
  });
});

describe("SftpPane 外部编辑器关联（M4a）", () => {
  const FILE = { name: "nginx.conf", is_dir: false, is_symlink: false, size: 100, mtime: 5, perms: "rw-r--r--" };

  beforeEach(() => {
    cleanup();
    localStorage.clear();
    invokeMock.mockReset();
    vi.useRealTimers();
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: [FILE], truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "editor_open") return "C:\\tmp\\edit\\nginx.conf";
      if (cmd === "editor_list") return [];
      return undefined;
    });
  });

  async function openEditor() {
    render(SftpPane, { sessionId: "s1" });
    const btn = await waitFor(() => {
      const b = document.querySelector('[data-testid="row-edit-nginx.conf"]');
      if (!b) throw new Error("edit button not rendered");
      return b as HTMLElement;
    });
    await fireEvent.click(btn);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("editor_open", { sessionId: "s1", remote: "./nginx.conf" }),
    );
  }

  it("打开后显示暂存路径与「正在编辑」条", async () => {
    await openEditor();
    await waitFor(() => expect(document.querySelector('[data-testid="sftp-editing"]')).not.toBeNull());
    expect(document.body.textContent).toContain("C:\\tmp\\edit\\nginx.conf");
  });

  it("无冲突的保存自动回传，不打扰用户", async () => {
    await openEditor();
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: [FILE], truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "editor_check") return { kind: "upload" };
      return undefined;
    });
    await waitFor(
      () => expect(invokeMock).toHaveBeenCalledWith("editor_upload", { sessionId: "s1", remote: "./nginx.conf" }),
      { timeout: 5000 },
    );
    // 自动回传不该弹确认
    expect(document.querySelector('[data-testid="confirm-dialog"]')).toBeNull();
  });

  it("冲突：弹确认且**不**先回传；确认后才覆盖", async () => {
    await openEditor();
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: [FILE], truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "editor_check")
        return {
          kind: "conflict",
          remote_at_open: { size: 100, mtime: 5 },
          remote_now: { size: 200, mtime: 9 },
        };
      return undefined;
    });
    await waitFor(() => expect(document.querySelector('[data-testid="confirm-dialog"]')).not.toBeNull(), {
      timeout: 5000,
    });
    expect(invokeMock).not.toHaveBeenCalledWith("editor_upload", expect.anything());
    expect(document.querySelector('[data-testid="confirm-msg"]')!.textContent).toContain("被改动过");
    await fireEvent.click(document.querySelector('[data-testid="confirm-ok"]')!);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("editor_upload", { sessionId: "s1", remote: "./nginx.conf" }),
    );
  });

  it("冲突时选「取消」= 停止编辑该文件（否则每 2 秒重复弹同一件事）", async () => {
    await openEditor();
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: [FILE], truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "editor_check")
        return {
          kind: "conflict",
          remote_at_open: { size: 100, mtime: 5 },
          remote_now: { size: 200, mtime: 9 },
        };
      return undefined;
    });
    await waitFor(() => expect(document.querySelector('[data-testid="confirm-dialog"]')).not.toBeNull(), {
      timeout: 5000,
    });
    await fireEvent.click(document.querySelector('[data-testid="confirm-cancel"]')!);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("editor_close", { sessionId: "s1", remote: "./nginx.conf" }),
    );
    expect(invokeMock).not.toHaveBeenCalledWith("editor_upload", expect.anything());
  });

  it("挂载时从后端恢复编辑列表（面板随标签切换重挂，编辑会话活在后端）", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: [FILE], truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "editor_list")
        return [{ session_id: "s1", remote: "./nginx.conf", local: "C:\\tmp\\x" },
                { session_id: "other", remote: "./not-mine", local: "C:\\tmp\\y" }];
      if (cmd === "editor_check") return { kind: "no_local_change" };
      return undefined;
    });
    render(SftpPane, { sessionId: "s1" });
    await waitFor(() => expect(document.querySelector('[data-testid="sftp-editing"]')).not.toBeNull());
    const bar = document.querySelector('[data-testid="sftp-editing"]')!.textContent!;
    expect(bar).toContain("./nginx.conf");
    expect(bar).not.toContain("not-mine"); // 别的会话的编辑不该出现在这个面板
  });

  it("软链不给编辑入口（改链还是改目标语义不明）", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list")
        return {
          entries: [{ ...FILE, name: "cfg.lnk", is_symlink: true }],
          truncated: false,
        };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "editor_list") return [];
      return undefined;
    });
    render(SftpPane, { sessionId: "s1" });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sftp_list", expect.anything()));
    expect(document.querySelector('[data-testid="row-edit-cfg.lnk"]')).toBeNull();
  });
});

/**
 * 拖拽入队的**中途取消**（2026-08-22 盘点补：M4a 出口原文含「拖拽中取消用例通过」，
 * 而 cancelEnqueue 与 sftp-enqueue-cancel 按钮此前零测试——落勾时核的是「注记点名的
 * 测试文件在不在」，没核「出口原文每个子句是否都有载体」，这一半就蒙混过去了）。
 *
 * 判据是三件事，缺一件这条就没验到点上：
 * ① 取消后**剩下的作业不再提交**（否则「取消」只是个安慰性按钮）；
 * ② 已提交的那些**照旧在队列里**，并明确告知用户——不能让人以为按了取消就什么都没发生；
 * ③ 取消前提交的数量如实反映在提示里。
 */
describe("SftpPane 拖拽入队中途取消（M4a 出口「拖拽中取消」）", () => {
  const D1 = { name: "proj", is_dir: true, is_symlink: false, size: 0, mtime: 3 };

  beforeEach(() => {
    cleanup();
    localStorage.clear();
    invokeMock.mockReset();
  });

  it("按下「停止入队」后：剩余作业不再提交，已提交的如实告知仍在队列里", async () => {
    // 12 个文件的目录：入队到第 1 件时点取消，后面 11 件都不该再提交
    const files = Array.from({ length: 12 }, (_, i) => `f${i}.bin`);
    let submitted = 0;
    let clickedCancel = false;

    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "local_list") return { entries: [D1], truncated: false };
      if (cmd === "sftp_list") return { entries: [], truncated: false };
      if (cmd === "local_walk") return { files, dirs: [], skipped_links: [] };
      if (cmd === "sftp_mkdir") return undefined;
      if (cmd === "transfer_submit") {
        submitted += 1;
        // 第一件提交后立刻点取消：模拟用户在进度条上按下按钮
        if (!clickedCancel) {
          clickedCancel = true;
          const btn = document.querySelector('[data-testid="sftp-enqueue-cancel"]');
          if (btn) (btn as HTMLElement).click();
        }
        return undefined;
      }
      return undefined;
    });

    render(SftpPane, { sessionId: "s1" });
    await waitFor(() => {
      if (!document.querySelector('[aria-label="本地文件列表"] li')) throw new Error("row not ready");
    });

    const data = new Map<string, string>([
      ["side", "local"],
      ["name", "proj"],
    ]);
    await fireEvent.drop(document.querySelector('section[aria-label="远程文件"]')!, {
      dataTransfer: { getData: (k: string) => data.get(k) ?? "" },
    });

    // 提示条出现即说明入队循环已收尾
    await waitFor(() =>
      expect(document.querySelector('[data-testid="sftp-sync-note"]')).not.toBeNull(),
    );

    // ① 取消真的止住了后续提交：远不到 12 件
    expect(submitted).toBeLessThan(files.length);
    expect(submitted).toBeGreaterThan(0); // 取消前那几件确实提交了

    // ②③ 文案必须说清「已提交的仍在传」，并带上数量——否则用户以为取消 = 什么都没发生
    const note = document.querySelector('[data-testid="sftp-sync-note"]')!.textContent!;
    expect(note).toContain("已停止入队");
    expect(note).toContain(String(submitted));
    expect(note).toContain("仍在传输队列里");
  });

  it("不点取消时全部提交（反向对照，防「恒取消」的假绿）", async () => {
    const files = ["a.bin", "b.bin", "c.bin"];
    let submitted = 0;
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "local_list") return { entries: [D1], truncated: false };
      if (cmd === "sftp_list") return { entries: [], truncated: false };
      if (cmd === "local_walk") return { files, dirs: [], skipped_links: [] };
      if (cmd === "transfer_submit") {
        submitted += 1;
        return undefined;
      }
      return undefined;
    });

    render(SftpPane, { sessionId: "s1" });
    await waitFor(() => {
      if (!document.querySelector('[aria-label="本地文件列表"] li')) throw new Error("row not ready");
    });
    const data = new Map<string, string>([
      ["side", "local"],
      ["name", "proj"],
    ]);
    await fireEvent.drop(document.querySelector('section[aria-label="远程文件"]')!, {
      dataTransfer: { getData: (k: string) => data.get(k) ?? "" },
    });
    await waitFor(() => expect(submitted).toBe(files.length));
    // 未取消时不该出现「已停止入队」
    const note = document.querySelector('[data-testid="sftp-sync-note"]');
    if (note) expect(note.textContent).not.toContain("已停止入队");
  });
});

/**
 * 本地栏的新建目录 / 重命名（M4b 第 15 项）。
 *
 * 评估结论见路线图 §6.3（`docs/roadmap.md`）：
 * 三个操作风险差着量级，所以**分拆**——新建与重命名做，删除不做。
 */
describe("SftpPane 本地栏（M4b 第 15 项）", () => {
  const SRC = SFTP_SOURCE;

  it("本地栏有新建目录与重命名两个按钮", () => {
    expect(SRC).toMatch(/data-testid="local-mkdir"/);
    expect(SRC).toMatch(/data-testid="local-rename"/);
  });

  it("它们调的是本地命令，不是远程那两个", () => {
    // 接错的话表现是「在本地栏点新建目录，结果在服务器上建了一个」——
    // 而那不会报错，用户要过一会儿才发现。
    expect(SRC).toMatch(/invoke\("local_mkdir", \{ parent: localPath/);
    expect(SRC).toMatch(/invoke\("local_rename", \{ parent: localPath/);
  });

  it("重命名要求恰好选中一项（多选时按钮禁用）", () => {
    expect(SRC).toMatch(/data-testid="local-rename" disabled=\{selectedLocal\.size !== 1\}/);
  });

  /**
   * **本地栏没有删除，而且这个缺席是刻意的。**
   *
   * 这条守卫防的是「顺手补齐对偶」——那看起来像在修一个不一致，实际是在加一个
   * 不进回收站的删除按钮。本地这一侧用户是在「用自己的电脑」，那上面有回收站，
   * 我们这里没有；且 `sftp.sandboxRoot` 管的是**下载落点**，对删除一句话都没说。
   */
  it("本地栏刻意没有删除按钮", () => {
    expect(SRC).not.toMatch(/data-testid="local-delete"/);
    expect(SRC).not.toMatch(/local_delete|local_remove|local_rmdir/);
    // 反向对照：远程栏**有**删除（否则这条会因为整个组件都没了而假绿）
    expect(SRC).toMatch(/removeSelected\(\)/);
  });
});

/* ══════════════════ M7.3 远端剪切/复制/粘贴 + 双击脚本 ══════════════════ */

/**
 * 出口标准①「远端剪切/复制/粘贴（同主机），冲突时问覆盖/保留两者/跳过」与
 * ②「双击 `.sh` 前展示脚本原文并让用户选前台/后台」的**接线面**。
 *
 * 纯逻辑（计划怎么算、结果怎么播报、路径怎么转义）在 lib/file-clipboard.test.ts 与
 * Rust 侧 fileops.rs 各自钉住；这里只测组件真的把它们串起来了——按钮存在且禁用条件对、
 * 冲突时先问再动手、双击脚本不会直接执行。
 */
describe("SftpPane 剪贴板与脚本运行（M7.3）", () => {
  const REMOTE = [
    { name: "dir1", is_dir: true, size: 0, mtime: 0, perms: "rwxr-xr-x", is_symlink: false },
    { name: "a.txt", is_dir: false, size: 3, mtime: 0, perms: "rw-r--r--", is_symlink: false },
    { name: "deploy.sh", is_dir: false, size: 9, mtime: 0, perms: "rwxr-xr-x", is_symlink: false },
  ];

  /** 远端一栏的行（本地栏也是 role="option"，必须按 aria-label 限定到远端那个列表）。 */
  function remoteRows(): HTMLElement[] {
    const list = document.querySelector('ul[aria-label="远程文件列表"]');
    return list ? ([...list.querySelectorAll('li[role="option"]')] as HTMLElement[]) : [];
  }
  const rowNamed = (n: string) => remoteRows().find((r) => r.textContent?.includes(n))!;
  const btn = (id: string) => document.querySelector(`[data-testid="${id}"]`) as HTMLButtonElement | null;

  beforeEach(async () => {
    cleanup();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: REMOTE, truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      return {};
    });
    const { clearClip } = await import("../lib/file-clipboard");
    clearClip();
  });

  async function mount(props: Record<string, unknown> = {}) {
    const view = render(SftpPane, { sessionId: "s1", ...props });
    await waitFor(() => expect(remoteRows().length).toBe(3));
    return view;
  }

  it("未选中时剪切/复制禁用；剪贴板为空时粘贴禁用并说明为什么", async () => {
    await mount();
    expect(btn("remote-cut")!.disabled).toBe(true);
    expect(btn("remote-copy")!.disabled).toBe(true);
    expect(btn("remote-paste")!.disabled).toBe(true);
    expect(btn("remote-paste")!.title).toContain("空");
  });

  it("复制把远端**完整路径**写进剪贴板（不是裸文件名）", async () => {
    await mount({ remoteDir: "/srv/app" });
    await waitFor(() => expect(remoteRows().length).toBe(3));
    await fireEvent.click(rowNamed("a.txt"));
    await fireEvent.click(btn("remote-copy")!);
    const { fileClip } = await import("../lib/file-clipboard");
    const { get } = await import("svelte/store");
    expect(get(fileClip)).toEqual({ sessionId: "s1", paths: ["/srv/app/a.txt"], cut: false });
  });

  it("剪切只是标记——此刻不发任何 IPC，文件还在原处", async () => {
    await mount();
    await fireEvent.click(rowNamed("a.txt"));
    invokeMock.mockClear();
    await fireEvent.click(btn("remote-cut")!);
    expect(invokeMock).not.toHaveBeenCalled();
    const { fileClip } = await import("../lib/file-clipboard");
    const { get } = await import("svelte/store");
    expect(get(fileClip)?.cut).toBe(true);
  });

  it("剪贴板属于别的会话时粘贴禁用，title 里写明为什么", async () => {
    const { setClip } = await import("../lib/file-clipboard");
    setClip("其它会话", ["/x/y.txt"], false);
    await mount();
    expect(btn("remote-paste")!.disabled).toBe(true);
    expect(btn("remote-paste")!.title).toContain("另一个会话");
  });

  it("无冲突时直接粘贴：探一次计划（skip）后原计划执行，不弹框", async () => {
    const { setClip } = await import("../lib/file-clipboard");
    setClip("s1", ["/other/a.txt"], false);
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: REMOTE, truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "fileops_plan") {
        return { ops: [{ src: "/other/a.txt", dst: "./a.txt", dst_name: "a.txt", renamed: false, cut: false }], skipped: [], same_path: [] };
      }
      if (cmd === "fileops_paste") return { done: [{ name: "a.txt", ok: true, error: "", renamed: false }], skipped: [], same_path: [] };
      return {};
    });
    await mount();
    await fireEvent.click(btn("remote-paste")!);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("fileops_paste", expect.anything()));
    expect(btn("paste-conflict")).toBeNull();
  });

  it("目标已存在时**先问再动手**：探测阶段绝不调 fileops_paste", async () => {
    const { setClip } = await import("../lib/file-clipboard");
    setClip("s1", ["/other/a.txt"], false);
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: REMOTE, truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "fileops_plan") return { ops: [], skipped: ["a.txt"], same_path: [] };
      return {};
    });
    await mount();
    await fireEvent.click(btn("remote-paste")!);
    await waitFor(() => expect(btn("paste-conflict")).not.toBeNull());
    expect(invokeMock).not.toHaveBeenCalledWith("fileops_paste", expect.anything());
    expect(document.querySelector('[data-testid="paste-conflict-names"]')!.textContent).toContain("a.txt");
    // 三档齐备且都没有被预选/自动执行
    for (const id of ["paste-overwrite", "paste-keep-both", "paste-skip"]) expect(btn(id)).not.toBeNull();
  });

  it("选「保留两者」后按该策略重算计划再执行", async () => {
    const { setClip } = await import("../lib/file-clipboard");
    setClip("s1", ["/other/a.txt"], false);
    const policies: unknown[] = [];
    invokeMock.mockImplementation(async (cmd?: string, args?: any) => {
      if (cmd === "sftp_list") return { entries: REMOTE, truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "fileops_plan") {
        policies.push(args.policy);
        return args.policy === "skip"
          ? { ops: [], skipped: ["a.txt"], same_path: [] }
          : { ops: [{ src: "/other/a.txt", dst: "./a (2).txt", dst_name: "a (2).txt", renamed: true, cut: false }], skipped: [], same_path: [] };
      }
      if (cmd === "fileops_paste") return { done: [{ name: "a (2).txt", ok: true, error: "", renamed: true }], skipped: [], same_path: [] };
      return {};
    });
    await mount();
    await fireEvent.click(btn("remote-paste")!);
    await waitFor(() => expect(btn("paste-keep-both")).not.toBeNull());
    await fireEvent.click(btn("paste-keep-both")!);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("fileops_paste", expect.anything()));
    expect(policies).toEqual(["skip", "keep_both"]);
    expect(btn("paste-conflict")).toBeNull();
  });

  it("冲突框「取消整次粘贴」什么都不做", async () => {
    const { setClip } = await import("../lib/file-clipboard");
    setClip("s1", ["/other/a.txt"], false);
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: REMOTE, truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "fileops_plan") return { ops: [], skipped: ["a.txt"], same_path: [] };
      return {};
    });
    await mount();
    await fireEvent.click(btn("remote-paste")!);
    await waitFor(() => expect(btn("paste-conflict-cancel")).not.toBeNull());
    await fireEvent.click(btn("paste-conflict-cancel")!);
    await waitFor(() => expect(btn("paste-conflict")).toBeNull());
    expect(invokeMock).not.toHaveBeenCalledWith("fileops_paste", expect.anything());
  });

  it("剪切粘贴成功后剪贴板清空——留着会让用户再粘一次而全部失败", async () => {
    const { setClip, fileClip } = await import("../lib/file-clipboard");
    const { get } = await import("svelte/store");
    setClip("s1", ["/other/a.txt"], true);
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: REMOTE, truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "fileops_plan") return { ops: [{ src: "/other/a.txt", dst: "./a.txt", dst_name: "a.txt", renamed: false, cut: true }], skipped: [], same_path: [] };
      if (cmd === "fileops_paste") return { done: [{ name: "a.txt", ok: true, error: "", renamed: false }], skipped: [], same_path: [] };
      return {};
    });
    await mount();
    await fireEvent.click(btn("remote-paste")!);
    await waitFor(() => expect(get(fileClip)).toBeNull());
  });

  it("双击 .sh **不执行**，先弹原文预览框", async () => {
    await mount();
    await fireEvent.dblClick(rowNamed("deploy.sh"));
    await waitFor(() => expect(btn("script-run")).not.toBeNull());
    expect(invokeMock).toHaveBeenCalledWith("sftp_read_text", { sessionId: "s1", path: "./deploy.sh" });
    expect(invokeMock).not.toHaveBeenCalledWith("fileops_run_script_background", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("fileops_foreground_command", expect.anything());
  });

  it("双击普通文件与目录不弹脚本框（目录照旧进去）", async () => {
    await mount();
    await fireEvent.dblClick(rowNamed("a.txt"));
    expect(btn("script-run")).toBeNull();
    await fireEvent.dblClick(rowNamed("dir1"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sftp_list", { sessionId: "s1", path: "./dir1" }));
    expect(btn("script-run")).toBeNull();
  });

  it("选前台 → 取命令并请 App 开新标签；不在本会话直接跑", async () => {
    const runs: string[] = [];
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: REMOTE, truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "sftp_read_text") return { text: "#!/bin/sh\necho hi\n", truncated: false, total_bytes: 18 };
      if (cmd === "fileops_foreground_command") return "sh ./deploy.sh";
      return {};
    });
    await mount({ onRunInNewTab: (c: string) => runs.push(c) });
    await fireEvent.dblClick(rowNamed("deploy.sh"));
    await waitFor(() => expect(btn("script-run-fg")).not.toBeNull());
    await fireEvent.click(btn("script-run-fg")!);
    await waitFor(() => expect(btn("script-run-ok")!.disabled).toBe(false));
    await fireEvent.click(btn("script-run-ok")!);
    await waitFor(() => expect(runs).toEqual(["sh ./deploy.sh"]));
    expect(invokeMock).not.toHaveBeenCalledWith("term_input", expect.anything());
  });

  it("选后台 → 走 nohup 命令，且写审计（script.run 是以远端身份执行任意代码）", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: REMOTE, truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "sftp_read_text") return { text: "echo hi\n", truncated: false, total_bytes: 8 };
      if (cmd === "fileops_run_script_background") return "nohup sh ./deploy.sh &";
      return {};
    });
    await mount();
    await fireEvent.dblClick(rowNamed("deploy.sh"));
    await waitFor(() => expect(btn("script-run-bg")).not.toBeNull());
    await fireEvent.click(btn("script-run-bg")!);
    // 「关掉本程序它仍在跑」必须在按下运行之前就看得到
    expect(document.querySelector('[data-testid="script-run-bg-warning"]')!.textContent).toContain("关掉本程序它仍在跑");
    await fireEvent.click(btn("script-run-ok")!);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("fileops_run_script_background", { sessionId: "s1", path: "./deploy.sh" }),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("audit_dangerous_action", expect.objectContaining({ kind: "script.run" })),
    );
  });

  it("没选前台/后台时「运行」是禁的（不得替用户默认）", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: REMOTE, truncated: false };
      if (cmd === "local_list") return { entries: [], truncated: false };
      if (cmd === "sftp_read_text") return { text: "echo hi\n", truncated: false, total_bytes: 8 };
      return {};
    });
    await mount();
    await fireEvent.dblClick(rowNamed("deploy.sh"));
    await waitFor(() => expect(btn("script-run-ok")).not.toBeNull());
    expect(btn("script-run-ok")!.disabled).toBe(true);
  });
});

/**
 * 远端导航必须真的重新列表（2026-09-04 修）。
 *
 * 缺陷本体在 `$effect(() => void refresh())` 与 `refresh()` 的 async 边界上：`$effect`
 * 只跟踪**第一个 `await` 之前**同步读到的状态，而 `remotePath`/`sessionId` 原来读在第一个
 * await 之后。于是远端一侧的每一种导航——双击进入子目录、点「上级」、点面包屑——都只改路径
 * 不重新列表：面包屑已经变了，下面还是上一个目录的内容。表现就是用户口中的「点了没反应」。
 *
 * 本地栏一直是好的（`localPath` 恰好读在 await 之前），既有的「档案目录晚到时跟随」也一直
 * 是绿的（它同时改两栏路径，靠 localPath 把 effect 拉起来、顺带读到了新的 remotePath）——
 * 这就是这个缺陷躲过既有测试的方式，所以下面三条**分别**钉远端的三种导航。
 *
 * 修法是把两栏路径与过滤器提成 `refresh` 的实参，在调用点同步读取。注意：只把 effect 那行
 * 改回 `refresh()`**证明不了**这三条守卫有效——默认参数 `lp = localPath, rp = remotePath`
 * 是在调用点同步求值的，依赖照样建立，测试照样全绿。有效的变异是把签名与调用点**一起**
 * 退回原样（实测 4 条转红：这三条 + 「双击目录照旧进去」）。
 */
describe("SftpPane 远端导航触发重新列表", () => {
  const REMOTE = [
    { name: "dir1", is_dir: true, size: 0, mtime: 0, perms: "rwxr-xr-x", is_symlink: false },
    { name: "a.txt", is_dir: false, size: 3, mtime: 0, perms: "rw-r--r--", is_symlink: false },
  ];
  const listedRemotePaths = () =>
    invokeMock.mock.calls.filter((c) => c[0] === "sftp_list").map((c) => (c[1] as { path: string }).path);

  beforeEach(() => {
    cleanup();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "sftp_list") return { entries: REMOTE, truncated: false };
      return { entries: [], truncated: false };
    });
  });

  async function mounted(props: Record<string, unknown> = {}) {
    render(SftpPane, { sessionId: "s1", ...props });
    await waitFor(() => expect(document.querySelectorAll('ul[aria-label="远程文件列表"] li[role="option"]').length).toBe(2));
  }
  const remoteRow = (n: string) =>
    [...document.querySelectorAll('ul[aria-label="远程文件列表"] li[role="option"]')]
      .find((r) => r.textContent?.includes(n)) as HTMLElement;

  it("双击子目录 → 用新路径重列", async () => {
    await mounted({ remoteDir: "/srv" });
    await waitFor(() => expect(listedRemotePaths()).toContain("/srv"));
    await fireEvent.dblClick(remoteRow("dir1"));
    await waitFor(() => expect(listedRemotePaths()).toContain("/srv/dir1"));
  });

  it("点「上级」→ 用上级路径重列", async () => {
    await mounted({ remoteDir: "/srv/app" });
    await waitFor(() => expect(listedRemotePaths()).toContain("/srv/app"));
    await fireEvent.click(document.querySelector('[data-testid="remote-up"]') as HTMLElement);
    await waitFor(() => expect(listedRemotePaths()).toContain("/srv"));
  });

  it("点面包屑 → 用那一级的路径重列", async () => {
    await mounted({ remoteDir: "/srv/app/logs" });
    await waitFor(() => expect(listedRemotePaths()).toContain("/srv/app/logs"));
    const crumb = [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === "app");
    expect(crumb, "远端面包屑里应有 app 这一级").toBeTruthy();
    await fireEvent.click(crumb!);
    await waitFor(() => expect(listedRemotePaths()).toContain("/srv/app"));
  });
});

/**
 * 「浮动窗口打开文件视图」的入口（M7.3 出口标准③）。
 *
 * 按钮本身很小，但它决定了这条工作流存不存在：终端与文件同屏看正是「文件管理器桌面化」
 * 要的东西。三条分别钉：给了回调才出现（没接线时不留一个点了没反应的按钮）、
 * 窗口内不再出现（再点一次只会置顶）、点它确实回调。
 */
describe("SftpPane 浮动入口（M7.3）", () => {
  beforeEach(() => {
    cleanup();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async () => ({ entries: [], truncated: false }));
  });

  const floatBtn = () => document.querySelector('[data-testid="remote-float"]') as HTMLButtonElement | null;

  it("接了 onFloat 才显示按钮", async () => {
    render(SftpPane, { sessionId: "s1" });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sftp_list", expect.anything()));
    expect(floatBtn()).toBeNull();
  });

  it("点它回调一次", async () => {
    const hits: number[] = [];
    render(SftpPane, { sessionId: "s1", onFloat: () => hits.push(1) });
    await waitFor(() => expect(floatBtn()).not.toBeNull());
    await fireEvent.click(floatBtn()!);
    expect(hits.length).toBe(1);
  });

  it("已经在浮动窗口里时不显示", async () => {
    render(SftpPane, { sessionId: "s1", floating: true, onFloat: () => {} });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sftp_list", expect.anything()));
    expect(floatBtn()).toBeNull();
  });
});
