/**
 * 会话树分组行右键菜单（2026-08-22 补）。
 *
 * 补这份测试的直接原因：M1/M4a 出口原文写的是「新建 / 重命名 / 删除」，而重命名此前
 * **全仓没有实现**——落勾时核的是「注记点名的测试在不在」，没核出口原文的每个子句。
 * 补完实现后发现 Sidebar 整个组件**一条组件测试都没有**：分组菜单的两项（删除、重命名）
 * 都只有实现、没有判据，改坏了不会有任何东西转红。
 *
 * 这里钉的是「菜单项 → 回调拿到正确路径」这一跳。改名的**语义**（子目录跟随、会话
 * group_path 迁移、同名拒绝）由 lib/folders.test.ts 的 renameFolder 一组覆盖。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import Sidebar from "./Sidebar.svelte";
import type { Profile } from "../lib/types";

// 显式标注 Profile[]：不标注时 host_key_policy 会宽成 string，与 HostKeyPolicy 联合类型不符
// （vitest 不查、svelte-check 查——它抓到过同一形状好几次）
const PROFILES: Profile[] = [
  {
    id: "p1",
    name: "生产机",
    host: "10.0.0.1",
    port: 22,
    username: "root",
    group_path: "工作/生产",
    auth: { vault_record: null, passphrase_vault_record: null, allow_agent: true, allow_kbd_interactive: true },
    jump: [],
    host_key_policy: "tofu",
    host_key_pins: [],
    env: {},
    term: { term: null, encoding: null, scrollback_lines: null },
    sftp: { local_dir: null, remote_dir: null, download_sandbox: null },
    ai_policy: {},
  },
];

/**
 * 右键点开某个分组行的菜单。
 *
 * 分组行没有 data-testid，是 `div.row.group`（role=treeitem），其 `title` 恰为**完整路径**
 * ——按 title 选比按可见文字选更准：可见文字只有末段（「生产」），两个不同父目录下的
 * 同名子目录会撞在一起。
 */
async function openGroupMenu(path: string) {
  const row = await waitFor(() => {
    const el = document.querySelector(`.row.group[title="${path}"]`);
    if (!el) {
      const seen = [...document.querySelectorAll(".row.group")].map((x) => x.getAttribute("title"));
      throw new Error(`找不到分组行 ${path}；现有分组行：${JSON.stringify(seen)}`);
    }
    return el as HTMLElement;
  });
  await fireEvent.contextMenu(row);
  await waitFor(() => {
    if (!document.querySelector('[data-testid="sidebar-groupmenu"]')) {
      throw new Error("分组菜单没弹出来");
    }
  });
  return row;
}

describe("Sidebar 分组行右键菜单", () => {
  beforeEach(() => cleanup());

  it("菜单同时提供「重命名」与「删除」两项", async () => {
    render(Sidebar, { profiles: PROFILES, manualFolders: ["工作", "工作/生产"] });
    await openGroupMenu("工作");
    expect(document.querySelector('[data-testid="groupmenu-rename"]')).not.toBeNull();
    expect(document.querySelector('[data-testid="groupmenu-del"]')).not.toBeNull();
  });

  it("点「重命名」把**该分组的路径**交给 onRenameFolder（不是名字、不是别的分组）", async () => {
    const onRenameFolder = vi.fn();
    render(Sidebar, {
      profiles: PROFILES,
      manualFolders: ["工作", "工作/生产"],
      onRenameFolder,
    });
    // 点二级分组「生产」：回调必须拿到完整路径「工作/生产」而不是末段「生产」，
    // 否则改名会作用到一个根级的同名目录上。
    await openGroupMenu("工作/生产");
    await fireEvent.click(document.querySelector('[data-testid="groupmenu-rename"]')!);
    expect(onRenameFolder).toHaveBeenCalledTimes(1);
    expect(onRenameFolder).toHaveBeenCalledWith("工作/生产");
  });

  it("点「删除」交给 onDeleteFolder，且不误触发重命名", async () => {
    const onDeleteFolder = vi.fn();
    const onRenameFolder = vi.fn();
    render(Sidebar, {
      profiles: PROFILES,
      manualFolders: ["工作"],
      onDeleteFolder,
      onRenameFolder,
    });
    await openGroupMenu("工作");
    await fireEvent.click(document.querySelector('[data-testid="groupmenu-del"]')!);
    expect(onDeleteFolder).toHaveBeenCalledWith("工作");
    expect(onRenameFolder).not.toHaveBeenCalled();
  });

  it("点任一项后菜单关闭（否则下一次右键会叠出两层）", async () => {
    render(Sidebar, { profiles: PROFILES, manualFolders: ["工作"], onRenameFolder: vi.fn() });
    await openGroupMenu("工作");
    await fireEvent.click(document.querySelector('[data-testid="groupmenu-rename"]')!);
    await waitFor(() =>
      expect(document.querySelector('[data-testid="sidebar-groupmenu"]')).toBeNull(),
    );
    // 变异验证留痕：把 `closeMenu` 改成空转，本条即红——判据有效。
    // 但**单独**删掉菜单项里的 `closeMenu()` 调用它仍绿，那不是判据不足：
    // 遮罩层自己也有 `onclick={closeMenu}`，点击冒泡上去照样关。两条路径互为冗余，
    // 去掉其一确实不改变行为。真正承重的是 `closeMenu` 本身。
  });

  it("破坏性项排在后面：重命名在删除之前（避免误点）", async () => {
    render(Sidebar, { profiles: PROFILES, manualFolders: ["工作"] });
    await openGroupMenu("工作");
    const items = [...document.querySelectorAll('[data-testid="sidebar-groupmenu"] li')];
    const iRename = items.findIndex((x) => x.getAttribute("data-testid") === "groupmenu-rename");
    const iDel = items.findIndex((x) => x.getAttribute("data-testid") === "groupmenu-del");
    expect(iRename).toBeGreaterThanOrEqual(0);
    expect(iDel).toBeGreaterThan(iRename);
  });
});
