import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import DIALOG_SRC from "./ProfileDialog.svelte?raw";

/**
 * 「指纹钉扎」的**可达性**测试（审计 P1 的前端那一半）。
 *
 * 后端把空钉从「弹一个点了也不收敛的假『密钥已变更』红框」改成硬拒之后，
 * 前端若仍然没有添加入口，这个策略就从「假安全」变成了「连不上」——
 * 用户在下拉里选得到它，却永远凑不齐一条钉。所以这里钉的不是某段渲染，
 * 而是「选了钉扎的用户能不能真的把一条钉存进 profile」这条完整通路。
 */

const calls: { cmd: string; args: any }[] = [];
let knownKeysResult: any = [];
let knownKeysError: string | null = null;
let vaultRecordsResult: any = [];
/** 非 null 时让 vault_list_secrets 抛错（2026-09-01：读列表失败此前被静默吞掉）。 */
let vaultListErr: string | null = null;
/** 共享凭据列表；null = 由 vaultRecordsResult 推导（见 mock）。 */
let sharedCredsResult: any = null;
/** credential_is_shared 的返回：非 null 时单选停在「使用共享凭据」。 */
let sharedNameResult: string | null = "共享";
let importResult: any = null;
let importError: string | null = null;
let vaultPutError: string | null = null;
/**
 * 设置键的桩值。**没配的键回落到调用方给的 fallback**——这与真实 `settingGet`
 * 的语义一致（未设置 ⇒ 用默认）。R31 的「新建档案吃全局默认」那几格靠它把
 * `hostkey.defaultPolicy` 设成 `strict` 再断言下拉框真的吃到。
 */
let settingsValues: Record<string, unknown> = {};
let saveWait: Promise<void> | null = null;
let failOwnPassword = false;

vi.mock("../lib/ipc", () => ({
  invoke: async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    if (cmd === "hostkey_known_keys") {
      if (knownKeysError) throw knownKeysError;
      return knownKeysResult;
    }
    if (cmd === "hostkey_import") {
      if (importError) throw importError;
      return importResult;
    }
    if (cmd === "vault_list_secrets") {
      if (vaultListErr) throw vaultListErr;
      return vaultRecordsResult;
    }
    // 凭据归属（2026-08-28）：共享列表默认取 vaultRecordsResult 的同一批，
    // 让既有的「类别置灰」用例在新界面上原样成立——那条语义没变，
    // 只是从「全局记录池选择器」搬到了「共享凭据选择器」。
    if (cmd === "credentials_shared_list")
      return sharedCredsResult ?? vaultRecordsResult.map((r: any) => ({
        record_id: r.id,
        name: `#${r.id} · ${r.label}`,
        kind: r.kind,
      }));
    if (cmd === "credential_is_shared") return sharedNameResult;
    if (cmd === "vault_put_secret") {
      if (vaultPutError) throw vaultPutError;
      return 99;
    }
    // M7.4 终端编码清单（后端 term_encodings）。给真列表而不是 undefined：
    // 下拉里没有对应 option 的话 bind:value 会退回空串，测的就成了「桩没给数据」。
    if (cmd === "serial_list_ports") return { ports: [{ name: "COM3", kind: "usb", description: "CP2102" }], note: "" };
    if (cmd === "serial_common_bauds") return [9600, 115200];
    if (cmd === "term_encodings")
      return [
        ["utf-8", "UTF-8（默认）"],
        ["gbk", "GBK / GB2312（简体中文）"],
        ["big5", "Big5（繁体中文）"],
      ];
    if (cmd === "profile_save") { if (saveWait) await saveWait; return "11111111-1111-1111-1111-111111111111"; }
    if (cmd === "profile_set_own_password" && failOwnPassword) throw new Error("保险库已锁定");
    return undefined;
  },
  settingGet: async (k: string, fallback: unknown) =>
    Object.prototype.hasOwnProperty.call(settingsValues, k) ? settingsValues[k] : fallback,
}));

import ProfileDialog from "./ProfileDialog.svelte";
import PROFILE_DIALOG_SRC from "./ProfileDialog.svelte?raw";

const KEY_A = {
  key_type: "ssh-ed25519",
  key_blob: "AAAAC3NzaC1lZDI1NTE5AAAAIAAA",
  fingerprint_sha256: "SHA256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  source: "tofu",
};
const KEY_B = {
  key_type: "ssh-rsa",
  key_blob: "AAAAB3NzaC1yc2EAAAADAQABBBB",
  fingerprint_sha256: "SHA256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  source: "imported",
};

/** 已存在的档案：走编辑路径，避免新建路径把 policy 回落成全局默认。 */
function profile(over: Record<string, any> = {}) {
  return {
    id: "22222222-2222-2222-2222-222222222222",
    name: "web-01",
    group_path: null,
    host: "10.0.0.1",
    port: 2222,
    username: "root",
    auth: {
      vault_record: null,
      passphrase_vault_record: null,
      allow_agent: false,
      allow_kbd_interactive: true,
      kbd_auto_answer_single: false,
    },
    jump: [],
    host_key_policy: "fingerprint_pinned",
    host_key_pins: [],
    env: {},
    ...over,
  } as any;
}

/** 打开对话框并切到「主机密钥」页签。 */
async function openHostKeyTab(initial: any) {
  render(ProfileDialog, { props: { open: true, initial } });
  await fireEvent.click(screen.getByText("主机密钥"));
}

describe("ProfileDialog · 钉扎指纹", () => {
  beforeEach(() => {
    calls.length = 0;
    knownKeysResult = [];
    knownKeysError = null;
    vaultRecordsResult = [];
    settingsValues = {};
  });

  // 空钉 + 钉扎策略在后端是**恒硬拒**。用户必须在保存前就知道，而不是等到下次
  // 连接失败——失败信息走 status 通道，未必被看见。
  it("策略=指纹钉扎且无钉 → 保存前就给出「无法建立」警告", async () => {
    await openHostKeyTab(profile());
    expect(screen.getByTestId("pf-hk-empty-pins").textContent).toContain("无法建立");
  });

  it("策略=TOFU 时不出这条警告（空钉是该策略的常态）", async () => {
    await openHostKeyTab(profile({ host_key_policy: "tofu" }));
    expect(screen.queryByTestId("pf-hk-empty-pins")).toBeNull();
  });

  // port 经 <input> 绑定可能变成字符串；Rust 侧 `port: u16` 对字符串直接反序列化失败，
  // 表现为选取器永远报「读取信任库失败」。类型必须是数字。
  it("打开选取器 → 按 host:port 查信任库，port 以数字下发", async () => {
    knownKeysResult = [KEY_A];
    await openHostKeyTab(profile());
    await fireEvent.click(screen.getByTestId("pf-add-pin"));
    const c = calls.find((x) => x.cmd === "hostkey_known_keys");
    expect(c).toBeTruthy();
    expect(c!.args).toEqual({ host: "10.0.0.1", port: 2222 });
    expect(typeof c!.args.port).toBe("number");
  });

  it("选一条候选 → 进入钉列表且警告消失；重复点击按 key_blob 去重", async () => {
    knownKeysResult = [KEY_A, KEY_B];
    await openHostKeyTab(profile());
    await fireEvent.click(screen.getByTestId("pf-add-pin"));

    const rows = screen.getAllByText("钉扎");
    await fireEvent.click(rows[0]);
    expect(screen.queryByTestId("pf-hk-empty-pins")).toBeNull();
    expect(screen.getAllByText(KEY_A.fingerprint_sha256).length).toBeGreaterThan(0);

    // 去重有两道：候选按钮的 disabled（真实浏览器里挡住点击）与 addPin 内按 key_blob 的判断。
    // 两道各自被**独立**钉住，靠的是 fireEvent 的一个性质：它直接派发事件，不执行浏览器的
    // activation behavior，因此 disabled 在这里拦不住第二次点击。于是——
    //   下一行断言只区分 disabled 绑定；再下面「点第二次后仍只有一条钉」只区分 addPin 的判断。
    // （变异实验：删 disabled → 只有本行红；删 addPin 判断 → 只有末尾的 pins 断言红。）
    expect((rows[0] as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(rows[0]);
    await fireEvent.click(screen.getByTestId("pf-save"));
    const saved = calls.find((x) => x.cmd === "profile_save");
    expect(saved!.args.profile.host_key_pins).toEqual([
      { key_blob: KEY_A.key_blob, fingerprint_sha256: KEY_A.fingerprint_sha256 },
    ]);
  });

  // 端到端：钉必须真的随 profile_save 落库，字段名与 Rust `HostKeyPin` 逐字一致
  // （key_blob + fingerprint_sha256，两者都无 default，缺一整份保存失败）。
  it("保存时钉随 profile 下发，字段形状与 Rust HostKeyPin 一致", async () => {
    knownKeysResult = [KEY_B];
    await openHostKeyTab(profile());
    await fireEvent.click(screen.getByTestId("pf-add-pin"));
    await fireEvent.click(screen.getByText("钉扎"));
    await fireEvent.click(screen.getByTestId("pf-save"));
    const saved = calls.find((x) => x.cmd === "profile_save");
    expect(saved!.args.profile.host_key_policy).toBe("fingerprint_pinned");
    expect(saved!.args.profile.host_key_pins).toEqual([
      { key_blob: KEY_B.key_blob, fingerprint_sha256: KEY_B.fingerprint_sha256 },
    ]);
  });

  // 查不到不是错误：新主机本就还没有记录。提示要指向**正确的下一步**（先连一次建立信任），
  // 而不是让用户以为功能坏了去手工找指纹粘贴——手填的指纹没有「本机确认过」这个来源。
  it("信任库无记录 → 提示先连一次，不报错、不发 profile_save", async () => {
    knownKeysResult = [];
    await openHostKeyTab(profile());
    await fireEvent.click(screen.getByTestId("pf-add-pin"));
    const picker = screen.getByTestId("pf-pin-picker");
    expect(picker.textContent).toContain("先以 TOFU 策略连接一次");
    expect(picker.textContent).not.toContain("失败");
  });

  it("host 未填 → 不查信任库，就地提示补主机地址", async () => {
    await openHostKeyTab(profile({ host: "" }));
    await fireEvent.click(screen.getByTestId("pf-add-pin"));
    expect(calls.some((x) => x.cmd === "hostkey_known_keys")).toBe(false);
    expect(screen.getByTestId("pf-pin-picker").textContent).toContain("请先填写主机地址");
  });

  it("信任库读取失败 → 展示错误，不静默给出空列表（否则会被误读为「这台主机没有记录」）", async () => {
    knownKeysError = "database is locked";
    await openHostKeyTab(profile());
    await fireEvent.click(screen.getByTestId("pf-add-pin"));
    const picker = screen.getByTestId("pf-pin-picker");
    expect(picker.textContent).toContain("读取信任库失败");
    expect(picker.textContent).not.toContain("先以 TOFU 策略连接一次");
  });
});

/* ────────────────────────────────────────────────────────────────────────────
 * R31：主机密钥「默认策略」设置的消费方
 *
 * 设置键 `hostkey.defaultPolicy` 的意义是：**新建**档案时预填哪个策略。它是
 * 「默认值」，不是「强制值」——编辑一个已有档案时，必须以档案自身的
 * `host_key_policy` 为准，全局默认不得覆盖它。这两半缺一不可：
 *   · 只有前半（新建吃默认）⇒ 设置能生效，但会悄悄改写用户既有档案的策略；
 *   · 只有后半（编辑不被覆盖）⇒ 设置形同虚设，新建永远是写死的 tofu。
 * ProfileDialog.svelte:185 的 `p?.host_key_policy ?? settingGet(...)` 就是这条
 * 分界；下面三格分别钉「新建吃默认」「新建无默认时回落 tofu」「编辑不被覆盖」。
 * ──────────────────────────────────────────────────────────────────────────── */
describe("ProfileDialog · R31 默认策略消费", () => {
  beforeEach(() => {
    calls.length = 0;
    settingsValues = {};
  });

  const policySelect = () => screen.getByTestId("pf-hk-policy") as HTMLSelectElement;

  it("新建档案 + 全局默认=strict → 策略下拉吃到 strict", async () => {
    settingsValues["hostkey.defaultPolicy"] = "strict";
    await openHostKeyTab(null); // initial=null ⇒ 新建路径
    expect(policySelect().value).toBe("strict");
  });

  it("新建档案 + 未设全局默认 → 回落到组件写死的 tofu", async () => {
    // settingsValues 里不配 hostkey.defaultPolicy ⇒ settingGet 返回 fallback "tofu"
    await openHostKeyTab(null);
    expect(policySelect().value).toBe("tofu");
  });

  it("编辑既有档案 + 全局默认=strict → 仍用档案自身值，不被默认覆盖", async () => {
    settingsValues["hostkey.defaultPolicy"] = "strict";
    // profile() 自带 host_key_policy="fingerprint_pinned"
    await openHostKeyTab(profile());
    expect(policySelect().value).toBe("fingerprint_pinned");
  });
});

/* ────────────────────────────────────────────────────────────────────────────
 * known_hosts 导入回执（审计2 #22 的前端一半）
 *
 * 后端把「跳过 N」拆成了九格，理由是那一个数字同时装着「本来就在库里」（无需动作）和
 * 「整行安全信息被丢掉」（信任根本没建立）。拆分只有走到界面上才有意义：如果前端仍然
 * 只印一句「导入 3」，那么 12 行通配主机没进来这件事对用户依旧不存在，他会以为整份
 * known_hosts 已经生效。enum-contract 那道门禁钉的是**字段名对得上**，这里钉的是
 * **每一类结局真的被说出来、说的还是那件事**。
 * ──────────────────────────────────────────────────────────────────────────── */

/** 九格全零的回执；`over` 只覆盖关心的那几格。 */
function summary(over: Record<string, number> = {}) {
  return {
    imported: 0,
    upgraded: 0,
    conflicted: 0,
    duplicate: 0,
    revoked: 0,
    cert_authority: 0,
    hashed: 0,
    pattern: 0,
    malformed: 0,
    ...over,
  };
}

/** 打开「主机密钥」页签，选一份 known_hosts 交上去，返回提示文案。 */
async function importKnownHosts(): Promise<string> {
  const { container } = render(ProfileDialog, { props: { open: true, initial: profile() } });
  await fireEvent.click(screen.getByText("主机密钥"));
  const input = container.querySelector('input[type="file"]') as HTMLInputElement;
  expect(input, "找不到 known_hosts 的文件输入：锚点失效了").toBeTruthy();
  const file = new File(["example.com ssh-ed25519 AAAA\n"], "known_hosts", { type: "text/plain" });
  await fireEvent.change(input, { target: { files: [file] } });
  await waitFor(() => expect(screen.queryByTestId("pf-toast")).not.toBeNull());
  return screen.getByTestId("pf-toast").textContent ?? "";
}

describe("ProfileDialog · known_hosts 导入回执", () => {
  beforeEach(() => {
    calls.length = 0;
    knownKeysResult = [];
    knownKeysError = null;
    vaultRecordsResult = [];
    importResult = summary();
    importError = null;
  });

  it("九格各自落在自己的措辞上（数字取 1–9，接错线当场可见）", async () => {
    importResult = summary({
      imported: 1,
      upgraded: 2,
      revoked: 3,
      duplicate: 4,
      conflicted: 5,
      cert_authority: 6,
      hashed: 7,
      pattern: 8,
      malformed: 9,
    });
    const t = await importKnownHosts();
    expect(t).toContain("导入 1");
    expect(t).toContain("择优替换 2");
    expect(t).toContain("已记为吊销 3");
    expect(t).toContain("已在库中 4");
    expect(t).toContain("冲突 5");
    expect(t).toContain("证书颁发机构 6");
    expect(t).toContain("哈希主机名 7");
    expect(t).toContain("通配/取反主机 8");
    expect(t).toContain("格式无法解析 9");
  });

  it("为零的类别一个字都不出现（否则每次导入都挂着一串「吊销 0」的噪声）", async () => {
    importResult = summary({ imported: 3 });
    const t = await importKnownHosts();
    expect(t, "零值类别泄漏进了提示").toBe("导入 3");
  });

  it("「本版本不支持」的三类必须明说这些行没有生效", async () => {
    // 这三类是**信任没有建立**：用户得知道要另想办法（换成非哈希条目、手工加主机名）。
    // 只报个数字，看上去和「已在库中 3」没有区别。
    importResult = summary({ cert_authority: 1, hashed: 2, pattern: 3 });
    const t = await importKnownHosts();
    for (const seg of t.split(" / ").filter((s) => !s.startsWith("导入"))) {
      expect(seg, `「${seg}」没说清这些行没有进来`).toContain("未导入");
    }
  });

  it("吊销要说清此后是硬拒绝——它是唯一一个会反转后续连接行为的类别", async () => {
    importResult = summary({ revoked: 2 });
    const t = await importKnownHosts();
    expect(t).toContain("已记为吊销 2");
    expect(t, "用户下次遇到这把密钥时点不了「接受」，界面必须先说").toContain("硬拒绝");
  });

  it("导入失败时给出失败提示，而不是一句看起来成功的「导入 0」", async () => {
    importError = "known_hosts 读取失败";
    const t = await importKnownHosts();
    expect(t).toContain("导入失败");
    expect(t, "失败却印着「导入 0」，用户会以为文件是空的").not.toMatch(/^导入 \d/);
  });
});

/* ────────────────────────────────────────────────────────────────────────────
 * 凭据类别的**可见性**（审计2 #20 的前端一半）
 *
 * 后端不再靠明文内容猜用途，改由记录自己声明的 SecretKind 分流，类别不匹配一律硬拒。
 * 这个改动把一类原本"能连上"的配置变成了"连接时被拒绝"，于是前端必须在**选之前**
 * 就把话说清楚——否则用户得到的是一句连接期的拒绝，而那时他既看不到记录的类别，
 * 也无从判断该改哪一栏。这几条钉的就是"说清楚"这件事本身。
 * ──────────────────────────────────────────────────────────────────────────── */

const REC_PW = { id: 1, kind: "password", label: "prod-pw" };
const REC_KEY = { id: 2, kind: "private_key", label: "prod-key" };
const REC_API = { id: 3, kind: "api_key", label: "claude-token" };
/** 后端将来新增的类别（或前端读到了更新版本的后端）：前端不认识它。 */
const REC_FUTURE = { id: 4, kind: "hardware_token", label: "yubikey" };

function optionsOf(testid: string): HTMLOptionElement[] {
  return [...screen.getByTestId(testid).querySelectorAll("option")];
}
/** id → option。共享凭据的名字里保留了 `#{id} ·` 前缀（见 mock），用它定位比用下标稳。 */
function optionFor(testid: string, id: number): HTMLOptionElement {
  const o = optionsOf(testid).find((x) => x.textContent?.trim().startsWith(`#${id} ·`));
  if (!o) throw new Error(`${testid} 里没有 id=${id} 的选项：候选=${optionsOf(testid).map((x) => x.textContent)}`);
  return o;
}

/** 绑着私钥记录的档案：打开后等它自动停到「使用保险库中的私钥」那一档，口令栏随之出现。 */
async function openKeyAuthTab(initial: any) {
  render(ProfileDialog, { props: { open: true, initial } });
  await fireEvent.click(screen.getByText("认证"));
  await waitFor(() => expect((screen.getByTestId("pf-cred-key") as HTMLInputElement).checked).toBe(true));
  await waitFor(() => expect(screen.getByTestId("pf-passphrase-select")).toBeTruthy());
}

async function openAuthTab(initial: any) {
  render(ProfileDialog, { props: { open: true, initial } });
  await fireEvent.click(screen.getByText("认证"));
  // 凭据归属（2026-08-28）：认证页默认停在「本连接专用密码」那一档，
  // 共享凭据选择器要切过去才在 DOM 里。这几条用例测的正是那个选择器。
  await fireEvent.click(screen.getByTestId("pf-cred-shared"));
  // vault_list_secrets 是异步的：不等它落地就断言，拿到的是只有「无」一项的空列表，
  // 于是所有"某某选项被禁用"的断言都会因为**根本没有那个选项**而恒真。
  await waitFor(() =>
    expect(optionsOf("pf-vault-select").length).toBe(vaultRecordsResult.length + 1),
  );
}

/* ────────────────────────────────────────────────────────────────────────────
 * 凭据归属（2026-08-28）：**默认每个连接一份自己的，共享是显式动作**
 *
 * 用户原话：「每一个连接我希望都有一个自己的凭据管理，除非另行设置，
 * 用户才可以把凭据抽出来同步到多个连接设置里」。
 *
 * 原实现只有一个「从全局记录池挑一条」的下拉框——那个形状在诱导共享，
 * 凭据看起来是「先建池子再挑一条」而不是「这个连接自己的密码」。
 * ──────────────────────────────────────────────────────────────────────────── */
describe("ProfileDialog · 凭据归属", () => {
  beforeEach(() => {
    calls.length = 0;
    knownKeysResult = [];
    knownKeysError = null;
    vaultRecordsResult = [REC_PW];
    sharedCredsResult = null;
    sharedNameResult = null; // 默认：没绑共享 → 停在「本连接专用」
  });

  it("新建连接默认停在「本连接专用密码」，而不是共享选择器", async () => {
    render(ProfileDialog, { props: { open: true, initial: undefined } });
    await fireEvent.click(screen.getByText("认证"));
    const own = screen.getByTestId("pf-cred-own") as HTMLInputElement;
    expect(own.checked, "默认必须是「本连接专用」——共享是显式动作，不是默认路径").toBe(true);
    expect(screen.getByTestId("pf-own-password")).toBeTruthy();
    // 共享选择器此时不该在 DOM 里（它是另一档）
    expect(screen.queryByTestId("pf-vault-select")).toBeNull();
  });

  it("已绑共享凭据的连接，打开时停在「使用共享凭据」档", async () => {
    sharedNameResult = "生产机 root";
    render(ProfileDialog, { props: { open: true, initial: profile({ auth: { vault_record: 1 } }) } });
    await fireEvent.click(screen.getByText("认证"));
    await waitFor(() =>
      expect((screen.getByTestId("pf-cred-shared") as HTMLInputElement).checked).toBe(true),
    );
  });

  it("输入专属密码保存 → 调 profile_set_own_password（存成只归本连接的记录）", async () => {
    render(ProfileDialog, { props: { open: true, initial: profile() } });
    await fireEvent.click(screen.getByText("认证"));
    await fireEvent.input(screen.getByTestId("pf-own-password"), {
      target: { value: "s3cret" },
    });
    await fireEvent.click(screen.getByText("保存"));
    await waitFor(() => {
      const c = calls.find((x) => x.cmd === "profile_set_own_password");
      expect(c, "专属密码没有被存起来").toBeTruthy();
      // 明文不进 IPC 参数：走 base64（与 vault_put_secret 同口径）
      expect(c?.args?.secretB64).toBe(btoa("s3cret"));
    });
  });

  it("密码留空不调存密码命令（留空 = 不修改，与占位文案一致）", async () => {
    render(ProfileDialog, { props: { open: true, initial: profile() } });
    await fireEvent.click(screen.getByText("认证"));
    await fireEvent.click(screen.getByText("保存"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "profile_save")).toBe(true));
    expect(
      calls.some((c) => c.cmd === "profile_set_own_password"),
      "留空却去存密码，会把已有的密码覆盖成空",
    ).toBe(false);
  });

  it("「设为共享」要起名字，并带着 recordId 调 credential_share", async () => {
    const prompt = vi.spyOn(window, "prompt").mockReturnValue("生产机 root");
    try {
      render(ProfileDialog, { props: { open: true, initial: profile({ auth: { vault_record: 7 } }) } });
      await fireEvent.click(screen.getByText("认证"));
      await fireEvent.click(await screen.findByTestId("pf-cred-share"));
      await waitFor(() => {
        const c = calls.find((x) => x.cmd === "credential_share");
        expect(c?.args).toMatchObject({ recordId: 7, name: "生产机 root" });
      });
    } finally {
      prompt.mockRestore();
    }
  });

  it("起名时按取消 → 不共享（prompt 返回 null）", async () => {
    const prompt = vi.spyOn(window, "prompt").mockReturnValue(null);
    try {
      render(ProfileDialog, { props: { open: true, initial: profile({ auth: { vault_record: 7 } }) } });
      await fireEvent.click(screen.getByText("认证"));
      await fireEvent.click(await screen.findByTestId("pf-cred-share"));
      await new Promise((r) => setTimeout(r, 10));
      expect(calls.some((c) => c.cmd === "credential_share")).toBe(false);
    } finally {
      prompt.mockRestore();
    }
  });

  it("已共享的连接给出「取消共享」入口", async () => {
    sharedNameResult = "生产机 root";
    render(ProfileDialog, { props: { open: true, initial: profile({ auth: { vault_record: 7 } }) } });
    await fireEvent.click(screen.getByText("认证"));
    // 停在 shared 档；切回 own 档才能看到共享状态与取消入口
    await waitFor(() =>
      expect((screen.getByTestId("pf-cred-shared") as HTMLInputElement).checked).toBe(true),
    );
    await fireEvent.click(screen.getByTestId("pf-cred-own"));
    await fireEvent.click(await screen.findByTestId("pf-cred-unshare"));
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "credential_unshare" && c.args?.recordId === 7)).toBe(true),
    );
  });

  it("没有任何共享凭据时给出说明，而不是一个空下拉框", async () => {
    sharedCredsResult = [];
    render(ProfileDialog, { props: { open: true, initial: profile() } });
    await fireEvent.click(screen.getByText("认证"));
    await fireEvent.click(screen.getByTestId("pf-cred-shared"));
    expect((await screen.findByTestId("pf-no-shared")).textContent).toContain("设为共享");
  });
});

describe("ProfileDialog · 凭据类别", () => {
  beforeEach(() => {
    calls.length = 0;
    knownKeysResult = [];
    knownKeysError = null;
    vaultRecordsResult = [REC_PW, REC_KEY, REC_API, REC_FUTURE];
    sharedCredsResult = null; // 共享列表由 vaultRecordsResult 推导（见 mock）
    // 这一组测的是共享凭据选择器，openAuthTab 会主动切到那一档；
    // sharedNameResult 与它无关，但**必须显式重置**——它是模块级变量，
    // 上一个 describe 的 beforeEach 把它设成了 null，不重置就跨组污染。
    sharedNameResult = null;
  });

  it("主凭据选取器：口令与私钥可选，API Key 与未知类别置灰并说明原因", async () => {
    await openAuthTab(profile());
    expect(optionFor("pf-vault-select", REC_PW.id).disabled).toBe(false);
    expect(
      optionFor("pf-vault-select", REC_KEY.id).disabled,
      "私钥被禁用了：所有密钥登录的档案都会变成选不了",
    ).toBe(false);
    expect(
      optionFor("pf-vault-select", REC_API.id).disabled,
      "api_key 可选 = 界面允许用户配一条后端必拒的凭据",
    ).toBe(true);
    expect(
      optionFor("pf-vault-select", REC_FUTURE.id).disabled,
      "不认识的类别必须按不可用处理（fail-closed），而不是当成可用",
    ).toBe(true);
    expect(optionFor("pf-vault-select", REC_API.id).textContent).toContain("不能用于 SSH 认证");
  });

  it("私钥口令选取器只列「密码」类记录：私钥 / API Key / 未知类别不再作为置灰诱饵出现", async () => {
    // 2026-09-02 用户第三次报「口令下拉展开后点不动」：下拉里列的是他的两把私钥，置灰、写着「不能作口令」。
    // 一个看得见、点不动的选项就是诱饵——不列，比置灰再解释强。口令栏只在主凭据是私钥时出现。
    await openKeyAuthTab(profile({ auth: { vault_record: REC_KEY.id } }));
    expect(optionFor("pf-passphrase-select", REC_PW.id).disabled).toBe(false);
    const texts = optionsOf("pf-passphrase-select").map((o) => o.textContent?.trim() ?? "");
    expect(texts.some((t) => t.startsWith(`#${REC_KEY.id} ·`)), "私钥出现在口令下拉里——那正是用户点不动的诱饵").toBe(false);
    expect(texts.some((t) => t.startsWith(`#${REC_API.id} ·`))).toBe(false);
    expect(texts.some((t) => t.startsWith(`#${REC_FUTURE.id} ·`))).toBe(false);
  });

  it("两个选取器都显示类别，且不认识的类别原样回显、不冒充成「密码」", async () => {
    await openAuthTab(profile());
    // 旧写法是 `kind === "private_key" ? "私钥" : kind === "api_key" ? "API Key" : "密码"`：
    // 任何不认识的串都被显示成「密码」——正是本次要消灭的那种猜测，而它显示的恰好是
    // 三种用途里最容易被随手选中的那一个。
    expect(optionFor("pf-vault-select", REC_FUTURE.id).textContent).toContain("hardware_token");
    expect(
      optionFor("pf-vault-select", REC_FUTURE.id).textContent,
      "未知类别被冒充成了「密码」",
    ).not.toContain("密码");
  });

  it("已存档案指向一条不可用的记录 → 出显式警告（置灰挡不住已经存在的选择）", async () => {
    await openAuthTab(profile({ auth: { vault_record: REC_API.id } }));
    expect(screen.getByTestId("pf-cred-kind-warn").textContent).toContain("API Key");
    expect(screen.queryByTestId("pf-pass-kind-warn")).toBeNull();
  });

  it("私钥口令栏指向一条私钥记录 → 出显式警告", async () => {
    await openKeyAuthTab(profile({ auth: { vault_record: REC_KEY.id, passphrase_vault_record: REC_KEY.id } }));
    expect(screen.getByTestId("pf-pass-kind-warn").textContent).toContain("私钥");
    expect(screen.queryByTestId("pf-cred-kind-warn"), "私钥是合法的主凭据，不该报警").toBeNull();
  });

  it("选的是可用记录时两条警告都不出（否则告警会变成永远亮着的噪声）", async () => {
    await openAuthTab(profile({ auth: { vault_record: REC_PW.id, passphrase_vault_record: REC_PW.id } }));
    expect(screen.queryByTestId("pf-cred-kind-warn")).toBeNull();
    expect(screen.queryByTestId("pf-pass-kind-warn")).toBeNull();
  });

  it("未选任何记录时不出警告（null 不是「类别不对」）", async () => {
    await openAuthTab(profile());
    expect(screen.queryByTestId("pf-cred-kind-warn")).toBeNull();
    expect(screen.queryByTestId("pf-pass-kind-warn")).toBeNull();
  });
});

/* ────────────────────────────────────────────────────────────────────────────
 * 行内录入的明文生命周期（审计2 #29）
 *
 * 「存入 Vault…」这个折叠区里，`newSecret` 承载的是用户刚敲进去的密码，或者整段
 * 私钥 PEM。此前它只在**下一次**打开对话框时才被 backfill 覆盖——而这个对话框挂在
 * App 顶层、活到进程结束，「下一次」可能永远不来。于是「填了一半按取消」「按 Escape」
 * 「收起折叠区改主意了」之后，明文一直留在组件状态里。
 *
 * 边界说清楚：JS 字符串不可变，这里做到的是**尽早丢掉引用**，不是清零；真正的清零
 * 在 Rust 侧（vault_put_secret 入口的 Zeroizing）。但「一直持有」和「立刻不持有」
 * 之间的差别是实打实的，也正是这一层唯一做得到的事。
 * ──────────────────────────────────────────────────────────────────────────── */
describe("ProfileDialog · 行内录入明文的生命周期", () => {
  beforeEach(() => {
    calls.length = 0;
    knownKeysResult = [];
    knownKeysError = null;
    vaultRecordsResult = [REC_PW];
    importResult = null;
    importError = null;
    vaultPutError = null;
  });

  /** 展开折叠区并填入一段明文；返回渲染句柄。 */
  async function typeSecret(view: any, secret = "hunter2-PLAINTEXT") {
    await fireEvent.click(screen.getByTestId("pf-vault-toggle"));
    await fireEvent.input(screen.getByTestId("pf-vault-label"), { target: { value: "临时" } });
    await fireEvent.input(screen.getByTestId("pf-vault-secret"), { target: { value: secret } });
    expect((screen.getByTestId("pf-vault-secret") as HTMLInputElement).value).toBe(secret);
    return view;
  }

  it("收起折叠区 = 放弃录入：再展开时明文已不在", async () => {
    render(ProfileDialog, { props: { open: true, initial: profile() } });
    await fireEvent.click(screen.getByText("认证"));
    await typeSecret(null);

    await fireEvent.click(screen.getByTestId("pf-vault-toggle")); // 收起
    expect(screen.queryByTestId("pf-vault-secret"), "收起后 fieldset 应当卸载").toBeNull();
    await fireEvent.click(screen.getByTestId("pf-vault-toggle")); // 再展开
    expect(
      (screen.getByTestId("pf-vault-secret") as HTMLInputElement).value,
      "收起只卸载了 DOM，明文还留在组件状态里",
    ).toBe("");
    expect((screen.getByTestId("pf-vault-label") as HTMLInputElement).value).toBe("");
  });

  it("关闭档案对话框 = 放弃录入：重新打开时折叠区已收起且明文已不在", async () => {
    const view = render(ProfileDialog, { props: { open: true, initial: profile() } });
    await fireEvent.click(screen.getByText("认证"));
    await typeSecret(view);

    await view.rerender({ open: false, initial: profile() }); // 取消 / Escape / 保存后关闭
    await view.rerender({ open: true, initial: profile() });
    await fireEvent.click(screen.getByText("认证"));

    expect(
      screen.queryByTestId("pf-vault-secret"),
      "重开后折叠区仍是展开的 —— 上一次录入的明文原样躺在里面",
    ).toBeNull();
    await fireEvent.click(screen.getByTestId("pf-vault-toggle"));
    expect((screen.getByTestId("pf-vault-secret") as HTMLInputElement).value).toBe("");
  });

  it("保存失败时明文保留在框里（不能把用户刚敲的密码在报错时一并吞掉）", async () => {
    render(ProfileDialog, { props: { open: true, initial: profile() } });
    await fireEvent.click(screen.getByText("认证"));
    await typeSecret(null, "will-fail");
    vaultPutError = "vault 未解锁";
    await fireEvent.click(screen.getByTestId("pf-vault-put"));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("保存失败"));
    expect(
      (screen.getByTestId("pf-vault-secret") as HTMLInputElement).value,
      "报错却把输入清空 = 用户得从头再敲一遍整段私钥",
    ).toBe("will-fail");
  });

  it("关闭时也清行内明文：清空调用写在 open 早退之前（源码守卫）", () => {
    // 行为断言到此为止了：对话框内容在 {#if open} 里，关闭即卸载，DOM 表面观测不到
    // 「关闭那一刻清没清」。V35 的 M15 变异（把清空挪到 open 早退之后）行为测试照常
    // 全绿——重新打开时新旧实现都会清。而审计2 #29 要的正是**关闭时**立即清理，
    // 不是拖到下次打开，所以这一条只能由源码守卫来钉（与 VaultDialog 的同款守卫
    // 一个口径）。
    const anchor = PROFILE_DIALOG_SRC.indexOf("clearVaultForm();");
    const start = PROFILE_DIALOG_SRC.lastIndexOf("$effect(() => {", anchor);
    expect(start, "找不到承载 clearVaultForm 的 $effect").toBeGreaterThan(-1);
    const end = PROFILE_DIALOG_SRC.indexOf("\n  });", start);
    const code = PROFILE_DIALOG_SRC.slice(start, end)
      .split("\n")
      .filter((l) => !l.trim().startsWith("//"))
      .join("\n");
    const reset = code.indexOf("clearVaultForm()");
    const earlyReturn = code.indexOf("if (!open)");
    expect(reset, "$effect 里找不到 clearVaultForm()").toBeGreaterThan(-1);
    expect(earlyReturn, "$effect 里找不到 open 早退分支").toBeGreaterThan(-1);
    expect(reset, "清空被 open 早退挡在后面 → 关闭后明文仍留在组件状态里").toBeLessThan(earlyReturn);
  });
});

/* ────────────────────────────────────────────────────────────────────────────
 * 跳板链编辑器（审计2 #23）
 *
 * 后端能力一直都在（connect.rs 的逐跳连接、逐跳凭据与信任断言），但 UI 只有一行
 * `initial?.jump ?? []` 透传：普通用户不能建不能改。导入剥离逐跳凭据与信任断言是
 * repo.rs 的既有不变式（有意为之），于是「配了跳板、想要维护」的用户没有任何可达入口。
 * 这里钉的是：逐跳回填、保存按行构建、无 UI 字段（口令记录/kbd 两旗）原样透传不蒸发、
 * 逐行校验定位到行号、增删移序、以及逐跳钉选取器走该跳自己的 host:port。
 * ──────────────────────────────────────────────────────────────────────────── */

const HOP_A = {
  host: "jump-a.example.com",
  port: 2201,
  username: "hopuser",
  auth: {
    vault_record: 1,
    passphrase_vault_record: 7,
    allow_agent: true,
    allow_kbd_interactive: false,
    kbd_auto_answer_single: true,
  },
  host_key_policy: "strict",
  host_key_pins: [],
};
const HOP_B = {
  host: "10.9.0.2",
  port: 22,
  username: "root",
  auth: {
    vault_record: null,
    passphrase_vault_record: null,
    allow_agent: false,
    allow_kbd_interactive: true,
    kbd_auto_answer_single: false,
  },
  host_key_policy: null,
  host_key_pins: [{ key_blob: KEY_B.key_blob, fingerprint_sha256: KEY_B.fingerprint_sha256 }],
};

/** 打开「跳板」页签。等待锚点是添加入口（空链时没有 pf-jump-0），行级锚点由各用例自等。 */
async function openJumpTab(initial: any) {
  const view = render(ProfileDialog, { props: { open: true, initial } });
  await fireEvent.click(screen.getByText("跳板"));
  await waitFor(() => expect(screen.queryByTestId("pf-jump-add")).not.toBeNull());
  return view;
}

function savedJump() {
  const saved = calls.find((x) => x.cmd === "profile_save");
  return saved!.args.profile.jump;
}

describe("ProfileDialog · 跳板链编辑器", () => {
  beforeEach(() => {
    calls.length = 0;
    knownKeysResult = [];
    knownKeysError = null;
    vaultRecordsResult = [REC_PW, REC_KEY];
  });

  it("既有跳板逐跳回填：主机/端口/用户名/凭据/策略/Agent/钉 各就各位", async () => {
    await openJumpTab(profile({ jump: [HOP_A, HOP_B] }));
    expect((screen.getByTestId("pf-jump-host-0") as HTMLInputElement).value).toBe("jump-a.example.com");
    expect((screen.getByTestId("pf-jump-port-0") as HTMLInputElement).value).toBe("2201");
    expect((screen.getByTestId("pf-jump-user-0") as HTMLInputElement).value).toBe("hopuser");
    expect((screen.getByTestId("pf-jump-vault-0") as HTMLSelectElement).value).toBe(String(REC_PW.id));
    expect((screen.getByTestId("pf-jump-policy-0") as HTMLSelectElement).value).toBe("strict");
    const cb0 = screen.getByTestId("pf-jump-0").querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(cb0.checked).toBe(true);
    const cb1 = screen.getByTestId("pf-jump-1").querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(cb1.checked).toBe(false);
    // 第二跳：vault 未选（value ""）、既有钉上屏
    expect((screen.getByTestId("pf-jump-vault-1") as HTMLSelectElement).value).toBe("");
    expect(screen.getByTestId("pf-jump-1").textContent).toContain(KEY_B.fingerprint_sha256);
  });

  it("保存按行构建 jump：无 UI 字段（口令记录/kbd 两旗/既有钉）原样透传，空策略发 null", async () => {
    await openJumpTab(profile({ jump: [HOP_A, HOP_B] }));
    await fireEvent.click(screen.getByTestId("pf-save"));
    expect(savedJump()).toEqual([
      {
        host: HOP_A.host, port: HOP_A.port, username: HOP_A.username,
        auth: {
          vault_record: 1, passphrase_vault_record: 7, allow_agent: true,
          allow_kbd_interactive: false, kbd_auto_answer_single: true,
        },
        host_key_policy: "strict", host_key_pins: [],
      },
      {
        host: HOP_B.host, port: HOP_B.port, username: HOP_B.username,
        auth: {
          vault_record: null, passphrase_vault_record: null, allow_agent: false,
          allow_kbd_interactive: true, kbd_auto_answer_single: false,
        },
        host_key_policy: null,
        host_key_pins: [{ key_blob: KEY_B.key_blob, fingerprint_sha256: KEY_B.fingerprint_sha256 }],
      },
    ]);
  });

  it("编辑生效：改主机/换凭据/改策略后保存，构建出的 jump 带着新值", async () => {
    await openJumpTab(profile({ jump: [HOP_A] }));
    await fireEvent.input(screen.getByTestId("pf-jump-host-0"), { target: { value: "new-jump.example.com" } });
    // select 走 change 事件（Svelte 的 select 绑定不听 input）
    await fireEvent.change(screen.getByTestId("pf-jump-vault-0"), { target: { value: String(REC_KEY.id) } });
    await fireEvent.change(screen.getByTestId("pf-jump-policy-0"), { target: { value: "tofu" } });
    await fireEvent.click(screen.getByTestId("pf-save"));
    const j = savedJump()[0];
    expect(j.host).toBe("new-jump.example.com");
    expect(j.auth.vault_record).toBe(REC_KEY.id);
    expect(j.host_key_policy).toBe("tofu");
    // 编辑过的行其余字段仍在（passphrase 等无 UI 字段不因编辑而丢）
    expect(j.auth.passphrase_vault_record).toBe(7);
    expect(j.auth.allow_kbd_interactive).toBe(false);
  });

  it("添加行后空主机 → 逐行校验拦下并定位到行号，不发 profile_save", async () => {
    await openJumpTab(profile());
    await fireEvent.click(screen.getByTestId("pf-jump-add"));
    await waitFor(() => expect(screen.queryByTestId("pf-jump-0")).not.toBeNull());
    // 新行默认端口 22
    expect((screen.getByTestId("pf-jump-port-0") as HTMLInputElement).value).toBe("22");
    await fireEvent.click(screen.getByTestId("pf-save"));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("跳板第 1 行：主机不能为空"));
    expect(calls.some((x) => x.cmd === "profile_save")).toBe(false);
  });

  it("端口 0 / 非数字 → 拦下（u16 反序列化对 0 后端也拒，前端先行定位）", async () => {
    await openJumpTab(profile());
    await fireEvent.click(screen.getByTestId("pf-jump-add"));
    await fireEvent.input(screen.getByTestId("pf-jump-host-0"), { target: { value: "jump-x" } });
    await fireEvent.input(screen.getByTestId("pf-jump-port-0"), { target: { value: "0" } });
    await fireEvent.click(screen.getByTestId("pf-save"));
    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain("跳板第 1 行：端口必须为 1–65535"),
    );
    expect(calls.some((x) => x.cmd === "profile_save")).toBe(false);
  });

  it("上移/下移改变链序：保存的 jump 数组按新顺序", async () => {
    await openJumpTab(profile({ jump: [HOP_A, HOP_B] }));
    await fireEvent.click(screen.getAllByLabelText("下移")[0]); // 行 0 下移
    await fireEvent.click(screen.getByTestId("pf-save"));
    expect(savedJump().map((h: any) => h.host)).toEqual([HOP_B.host, HOP_A.host]);
  });

  it("删除行：该跳从链中移除，余下各跳顺序不变", async () => {
    await openJumpTab(profile({ jump: [HOP_A, HOP_B] }));
    await fireEvent.click(screen.getAllByLabelText("删除跳板")[0]);
    await fireEvent.click(screen.getByTestId("pf-save"));
    expect(savedJump().map((h: any) => h.host)).toEqual([HOP_B.host]);
  });

  it("上限 8 与后端 JUMP_MAX 同源：满 8 行添加入口禁用", async () => {
    await openJumpTab(profile({ jump: Array.from({ length: 8 }, (_, k) => ({ ...HOP_A, host: `h${k}` })) }));
    expect((screen.getByTestId("pf-jump-add") as HTMLButtonElement).disabled).toBe(true);
  });

  it("7 行未到上限：添加入口仍可用", async () => {
    await openJumpTab(profile({ jump: Array.from({ length: 7 }, (_, k) => ({ ...HOP_A, host: `h${k}` })) }));
    await waitFor(() => expect(screen.queryByTestId("pf-jump-6")).not.toBeNull());
    expect((screen.getByTestId("pf-jump-add") as HTMLButtonElement).disabled).toBe(false);
  });

  it("逐跳钉选取器：按该跳自己的 host:port 查信任库，选中后写进该跳 pins 并随保存下发", async () => {
    knownKeysResult = [KEY_A];
    await openJumpTab(profile({ jump: [HOP_A, HOP_B] }));
    await fireEvent.click(screen.getByTestId("pf-jump-pinbtn-0"));
    await waitFor(() => expect(screen.queryByTestId("pf-hop-pin-picker")).not.toBeNull());
    const c = calls.find((x) => x.cmd === "hostkey_known_keys");
    expect(c).toBeTruthy();
    expect(c!.args).toEqual({ host: "jump-a.example.com", port: 2201 });
    expect(typeof c!.args.port).toBe("number"); // 与目标机选取器同一陷阱：字符串 port 后端直接失败
    await fireEvent.click(screen.getByText("钉扎"));
    await fireEvent.click(screen.getByTestId("pf-save"));
    const j = savedJump();
    expect(j[0].host_key_pins).toEqual([
      { key_blob: KEY_A.key_blob, fingerprint_sha256: KEY_A.fingerprint_sha256 },
    ]);
    // 别的跳的钉不受影响
    expect(j[1].host_key_pins).toEqual([
      { key_blob: KEY_B.key_blob, fingerprint_sha256: KEY_B.fingerprint_sha256 },
    ]);
  });

  it("钉扎策略 + 本跳无钉 → 保存前警告该跳无法建立（与目标机同款陷阱）", async () => {
    await openJumpTab(profile({ jump: [{ ...HOP_A, host_key_policy: "fingerprint_pinned" }] }));
    expect(screen.getByTestId("pf-jump-pins-warn-0").textContent).toContain("无法建立");
  });

  it("该跳主机未填 → 钉选取器就地提示，不查信任库", async () => {
    await openJumpTab(profile({ jump: [{ ...HOP_A, host: "" }] }));
    await fireEvent.click(screen.getByTestId("pf-jump-pinbtn-0"));
    expect(calls.some((x) => x.cmd === "hostkey_known_keys")).toBe(false);
    expect(screen.getByTestId("pf-hop-pin-picker").textContent).toContain("请先填写该跳的主机地址");
  });

  it("该跳凭据指向不可用类别 → 出显式警告（置灰挡不住已经存在的选择）", async () => {
    vaultRecordsResult = [REC_PW, REC_KEY, REC_API];
    await openJumpTab(profile({ jump: [{ ...HOP_A, auth: { ...HOP_A.auth, vault_record: REC_API.id } }] }));
    expect(screen.getByText(/不能作为 SSH 主凭据/).textContent).toContain("连接这一跳时后端会直接拒绝");
  });
});

/* 审计2 #24：Agent 开关文案。原「允许 Agent 转发」把「用本地 agent 做客户端公钥认证」
 * 说成了「SSH agent forwarding」（远端能用本地 agent），是错误的安全预期。这是一条纯文案
 * 修复，行为断言无从下手——源码守卫直接钉文案本身（新句在、旧句绝迹）。 */
describe("审计2 #24：Agent 开关文案", () => {
  it("文案=「使用本地 SSH Agent 认证」，误导性旧文案不残留", () => {
    // 剥掉注释再断言：修复说明注释里**引用**了旧文案（考古价值），那不是 UI 文案；
    // 不剥注释，这条守卫会被自己的注释内容打红（也拦不住把旧文案藏进注释的变异）。
    const code = PROFILE_DIALOG_SRC
      .replace(/<!--[\s\S]*?-->/g, "")
      .replace(/(^|[^:])\/\/.*$/gm, "$1");
    expect(code).toContain("使用本地 SSH Agent 认证");
    expect(code).not.toContain("允许 Agent 转发");
  });
});

describe("审计2 #37：known_hosts 导入尺寸闸（纵深防御）", () => {
  /**
   * 真正的闸在 Rust（import_known_hosts 解析前量尺寸）。前端这一层是「别把超大文件
   * 整份读进内存、推过 IPC 再被后端拒」的纵深防御——删掉它（或挪到 file.text() 之后）
   * 编译照过、功能照常，只有这里红。与 action-wiring.test.ts 里 App.svelte 那条
   * 同一口径：钉形状（闸在 read 之前、拒绝即 return），数值由 Rust 侧主守。
   */
  it("导入文件在 file.text() 之前量尺寸：超过 8 MiB 直接拒绝", () => {
    const code = DIALOG_SRC.replace(/(^|[^:])\/\/.*$/gm, "$1"); // 剥行注释：闸上方的说明写着同一件事，不算数
    const gate = code.match(/file\.size > 8 \* 1024 \* 1024/g) ?? [];
    expect(gate.length).toBe(1);
    const gatePos = code.indexOf("file.size > 8 * 1024 * 1024");
    const readPos = code.indexOf("await file.text()");
    expect(gatePos).toBeGreaterThan(-1);
    expect(readPos).toBeGreaterThan(-1);
    expect(gatePos).toBeLessThan(readPos); // 顺序颠倒 = 闸白设，内存峰值已打出来
    const between = code.slice(gatePos, readPos);
    expect(between).toContain("return"); // 拒绝后必须直接返回，不得继续读文件
  });
});

/* ------------------------------------------------------------------------------
 * 凭据下拉「为什么是空的」必须说出来（用户 2026-09-01 两次报「下拉无法选择」）
 *
 * 三种情况在界面上长得一模一样——下拉里只有一个「无」——但修法完全不同：
 * ① 读列表失败（最常见：保险库锁着）→ 该去解锁，不是去建记录；
 * ② 一条记录都没有 → 该去建一条；
 * ③ 有记录但都不是密码类 → 私钥口令只能用密码类（拿私钥解私钥无意义）。
 *
 * 此前 refreshVaultRecords 的 catch 是 `vaultRecords = []`——**静默吞掉**，
 * 三种情况都退化成一个空下拉，用户只能得出「这控件坏了」的结论。
 * ---------------------------------------------------------------------------- */
describe("ProfileDialog 凭据列表的三态", () => {
  /** 轻量夹具：只开到认证页，**不等列表落地**——失败场景永远等不到，
   *  而这几条用例要测的正是「等不到的时候界面说什么」。 */
  async function openAuth(): Promise<void> {
    render(ProfileDialog, { props: { open: true, initial: undefined } });
    await fireEvent.click(screen.getByText("认证"));
  }

  beforeEach(() => {
    vaultListErr = null;
    vaultRecordsResult = [];
  });

  it("读列表失败 → 说出原因并给「重试」，不是留一个空下拉", async () => {
    vaultListErr = "vault 未解锁";
    await openAuth();
    const box = await waitFor(() => screen.getByTestId("pf-vault-list-error"));
    expect(box.textContent, "保险库锁着要指向解锁，而不是让用户去建记录").toContain("保险库未解锁");
    expect(box.querySelector("button")?.textContent).toContain("重试");
  });

  it("非「未解锁」的失败原样带出后端错误（排障要的是原文）", async () => {
    vaultListErr = "database is locked";
    await openAuth();
    const box = await waitFor(() => screen.getByTestId("pf-vault-list-error"));
    expect(box.textContent).toContain("database is locked");
  });

  it("真的一条记录都没有 → 指向「存入 Vault…」，不与失败混为一谈", async () => {
    vaultRecordsResult = [];
    await openAuth();
    await waitFor(() => expect(screen.getByTestId("pf-vault-empty")).toBeTruthy());
    expect(screen.queryByTestId("pf-vault-list-error")).toBeNull();
  });

  it("有记录且有密码类 → 三句提示一句都不出现（有可用项时它们是噪声）", async () => {
    vaultRecordsResult = [{ id: 1, kind: "password", label: "prod-pw" }];
    await openAuth();
    await fireEvent.click(screen.getByTestId("pf-cred-key")); // 口令下拉只在私钥档出现（2026-09-02）
    await waitFor(() =>
      expect(optionsOf("pf-passphrase-select").length).toBe(2),
    );
    expect(screen.queryByTestId("pf-vault-list-error")).toBeNull();
    expect(screen.queryByTestId("pf-vault-empty")).toBeNull();
    expect(screen.queryByTestId("pf-no-passphrase-record")).toBeNull();
  });

  it("password 类别的记录在私钥口令下拉里**可选**（用户报的正是它点不动）", async () => {
    vaultRecordsResult = [
      { id: 1, kind: "password", label: "root@139.129.33.6" },
      { id: 2, kind: "password", label: "123" },
    ];
    await openAuth();
    await fireEvent.click(screen.getByTestId("pf-cred-key")); // 口令下拉只在私钥档出现（2026-09-02）
    await waitFor(() => expect(optionsOf("pf-passphrase-select").length).toBe(3));
    for (const id of [1, 2]) {
      expect(
        optionFor("pf-passphrase-select", id).disabled,
        `#${id} 是 password 类别，必须可选`,
      ).toBe(false);
    }
  });
});

/* ─────────────────────────────────────────────────────────────────────────────
 * 私钥档（2026-09-02）：用户第三次报「私钥口令下拉展开后点不动」。真相是两件事叠在一起：
 * ① 口令下拉把他的两把私钥列出来又置灰——诱饵（上面那组已改成不列）；
 * ② 他真正想做的「把这把私钥配给这个连接」在界面上没有入口：共享列表只有显式共享过的记录，
 *    存进保险库的私钥只在口令下拉里露过面。现在私钥有自己的一档。
 * ───────────────────────────────────────────────────────────────────────────── */
describe("ProfileDialog · 私钥档（使用保险库中的私钥）", () => {
  beforeEach(() => {
    calls.length = 0;
    knownKeysResult = [];
    knownKeysError = null;
    vaultRecordsResult = [REC_PW, REC_KEY, REC_API];
    sharedCredsResult = [];
    sharedNameResult = null;
  });

  it("第三档只列私钥记录；选中一条保存 → vault_record 指向它", async () => {
    render(ProfileDialog, { props: { open: true, initial: profile() } });
    await fireEvent.click(screen.getByText("认证"));
    await fireEvent.click(screen.getByTestId("pf-cred-key"));
    await waitFor(() => expect(optionsOf("pf-key-select").length).toBe(2)); // 「无」+ 1 把私钥
    const texts = optionsOf("pf-key-select").map((o) => o.textContent?.trim() ?? "");
    expect(texts.some((t) => t.startsWith(`#${REC_KEY.id} ·`))).toBe(true);
    expect(texts.some((t) => t.startsWith(`#${REC_PW.id} ·`)), "密码不该出现在私钥档").toBe(false);
    expect(texts.some((t) => t.startsWith(`#${REC_API.id} ·`))).toBe(false);
    await fireEvent.change(screen.getByTestId("pf-key-select"), { target: { value: String(REC_KEY.id) } });
    await fireEvent.click(screen.getByTestId("pf-save"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "profile_save")).toBe(true));
    const save = calls.find((c) => c.cmd === "profile_save")!;
    expect(save.args.profile.auth.vault_record).toBe(REC_KEY.id);
  });

  it("档案已绑私钥 → 打开时自动停在私钥档，私钥口令栏出现，不显示「已保存」的密码框", async () => {
    await openKeyAuthTab(profile({ auth: { vault_record: REC_KEY.id } }));
    expect(screen.queryByTestId("pf-own-password"), "绑的是私钥，密码档的「已保存（留空 = 不修改）」是张冠李戴").toBeNull();
    expect(optionsOf("pf-key-select").some((o) => o.selected && o.textContent?.startsWith(`#${REC_KEY.id} ·`))).toBe(true);
  });

  it("密码档不显示私钥口令栏：它只对私钥有意义，密码用户面前不该有个点不动的下拉", async () => {
    render(ProfileDialog, { props: { open: true, initial: profile() } });
    await fireEvent.click(screen.getByText("认证"));
    await waitFor(() => expect((screen.getByTestId("pf-cred-own") as HTMLInputElement).checked).toBe(true));
    await waitFor(() => expect(calls.some((c) => c.cmd === "vault_list_secrets")).toBe(true));
    expect(screen.queryByTestId("pf-passphrase-select")).toBeNull();
  });

  it("保险库里没有私钥 → 私钥档说清去哪儿存，而不是一个只有「无」的下拉", async () => {
    vaultRecordsResult = [REC_PW];
    render(ProfileDialog, { props: { open: true, initial: profile() } });
    await fireEvent.click(screen.getByText("认证"));
    await fireEvent.click(screen.getByTestId("pf-cred-key"));
    await waitFor(() => expect(screen.getByTestId("pf-no-key").textContent).toContain("存入 Vault"));
  });
});

/**
 * 终端编码栏（M7.4）。
 *
 * 修前这一栏是**死配置**：一个 `disabled` 的输入框恒显 "UTF-8"，而 `term.encoding` 在整个
 * 仓库里没有任何消费方——用户在这儿什么都做不了，做了也不会生效。国产板子输出 GBK 时
 * 满屏乱码而界面上写着「v1 仅 UTF-8」。
 *
 * 两条判据分别对着这两半：① 这一栏是可用的、选了会存进档案；② 清单来自后端
 *（能不能解出来是 Rust 侧 StreamDecoder 说了算，前端另写一份迟早分叉成「选了却报错」）。
 */
describe("ProfileDialog · 终端编码（M7.4）", () => {
  beforeEach(() => {
    calls.length = 0;
    knownKeysResult = [];
    vaultRecordsResult = [];
    settingsValues = {};
  });

  const encSel = () => document.querySelector('[data-testid="pf-encoding"]') as HTMLSelectElement;

  async function openTerminalTab(initial?: any) {
    render(ProfileDialog, { props: { open: true, initial } });
    await fireEvent.click(screen.getByText("终端"));
    await waitFor(() => expect(encSel()).not.toBeNull());
  }

  it("编码栏可用（不再是恒显 UTF-8 的死输入框）", async () => {
    await openTerminalTab(profile());
    expect(encSel().tagName).toBe("SELECT");
    expect(encSel().disabled).toBe(false);
  });

  it("清单来自后端 term_encodings，前端不写死", async () => {
    await openTerminalTab(profile());
    await waitFor(() => expect(calls.some((c) => c.cmd === "term_encodings")).toBe(true));
    expect(PROFILE_DIALOG_SRC).not.toMatch(/"gb18030"[\s\S]{0,40}"big5"/);
  });

  it("回填已存的编码", async () => {
    await openTerminalTab(profile({ term: { term: null, encoding: "gbk", scrollback_lines: null } }));
    expect(encSel().value).toBe("gbk");
  });

  it("选定后保存进档案的 term.encoding（留空 = null，不编造默认值）", async () => {
    await openTerminalTab(profile());
    expect(encSel().value).toBe("");
    await fireEvent.click(screen.getByText("保存"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "profile_save")).toBe(true));
    const saved = calls.filter((c) => c.cmd === "profile_save").pop()!;
    expect(saved.args.profile.term.encoding).toBeNull();
  });

  it("界面上要说明连上之后还能临时切、且不需要重连", async () => {
    await openTerminalTab(profile());
    expect(document.body.textContent).toContain("不需要重连");
  });
});

/** 清单里没有的标签（手改过 JSON / 清单取不到）不得被静默丢弃：
 *  下拉退回默认 + 用户按保存 = 他原来的设置没了，而界面上一句提示都没有。 */
describe("ProfileDialog · 编码：档案里的未知标签不被静默丢弃", () => {
  beforeEach(() => {
    calls.length = 0;
    knownKeysResult = [];
    vaultRecordsResult = [];
    settingsValues = {};
  });

  it("列出并选中它，保存后原样回去", async () => {
    render(ProfileDialog, {
      props: { open: true, initial: profile({ term: { term: null, encoding: "koi8-r", scrollback_lines: null } }) },
    });
    await fireEvent.click(screen.getByText("终端"));
    const sel = () => document.querySelector('[data-testid="pf-encoding"]') as HTMLSelectElement;
    await waitFor(() => expect(document.querySelector('[data-testid="pf-encoding-extra"]')).not.toBeNull());
    expect(sel().value).toBe("koi8-r");
    await fireEvent.click(screen.getByText("保存"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "profile_save")).toBe(true));
    expect(calls.filter((c) => c.cmd === "profile_save").pop()!.args.profile.term.encoding).toBe("koi8-r");
  });
});

/**
 * 串口档案（M7.4）。
 *
 * 两条要害：① 串口没有主机/用户名/端口，那三栏必须**不在**——留着会让用户以为要填点什么，
 * 而填什么都不影响它连到哪里；② 端口名可**输入**而不只是选——容器里的 socat 伪终端、
 * Linux 上没被 sysfs 扫到的口都不会出现在枚举列表里，只能选就等于连不上它们。
 */
describe("ProfileDialog · 串口档案（M7.4）", () => {
  beforeEach(() => {
    calls.length = 0;
    knownKeysResult = [];
    vaultRecordsResult = [];
    settingsValues = {};
  });

  const q = (id: string) => document.querySelector(`[data-testid="${id}"]`) as HTMLElement | null;

  async function openSerial() {
    render(ProfileDialog, { props: { open: true, initial: undefined } });
    await fireEvent.change(q("pf-protocol")!, { target: { value: "serial" } });
    await waitFor(() => expect(q("pf-serial-port")).not.toBeNull());
  }

  it("选了串口之后，主机/用户名那几栏消失，串口参数出现", async () => {
    await openSerial();
    expect(q("pf-host")).toBeNull();
    for (const id of ["pf-serial-port", "pf-serial-baud", "pf-serial-databits", "pf-serial-parity", "pf-serial-stopbits", "pf-serial-flow"]) {
      expect(q(id), id).not.toBeNull();
    }
  });

  it("端口列表来自后端，且拿不到时给的是说明而不是错误框", async () => {
    await openSerial();
    await waitFor(() => expect(calls.some((c) => c.cmd === "serial_list_ports")).toBe(true));
    expect(q("pf-serial-hint")!.textContent).toBeTruthy();
  });

  it("端口名可以直接打（枚举是便利不是白名单）", async () => {
    await openSerial();
    expect((q("pf-serial-port") as HTMLInputElement).tagName).toBe("INPUT");
  });

  it("认证/跳板/主机密钥/SFTP 页签在串口档案上收起来", async () => {
    await openSerial();
    for (const label of ["认证", "跳板", "主机密钥", "SFTP"]) {
      expect(screen.queryByText(label), label).toBeNull();
    }
    // 反向对照：终端页签仍在（否则这条会因为整排页签都没了而假绿）
    expect(screen.queryByText("终端")).not.toBeNull();
  });

  it("保存时把串口参数写进档案", async () => {
    await openSerial();
    await fireEvent.input(q("pf-name")!, { target: { value: "板子" } });
    await fireEvent.input(q("pf-serial-port")!, { target: { value: "/dev/ttyUSB0" } });
    await fireEvent.input(q("pf-serial-baud")!, { target: { value: "9600" } });
    await fireEvent.change(q("pf-serial-parity")!, { target: { value: "even" } });
    await fireEvent.click(screen.getByText("保存"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "profile_save")).toBe(true));
    const saved = calls.filter((c) => c.cmd === "profile_save").pop()!;
    expect(saved.args.profile.protocol).toBe("serial");
    expect(saved.args.profile.serial).toMatchObject({
      port: "/dev/ttyUSB0",
      baud: 9600,
      parity: "even",
      data_bits: "eight",
      stop_bits: "one",
      flow: "none",
    });
  });

  it("回填已存的串口档案", async () => {
    render(ProfileDialog, {
      props: {
        open: true,
        initial: profile({
          protocol: "serial",
          serial: { port: "COM7", baud: 250000, data_bits: "seven", parity: "odd", stop_bits: "two", flow: "hardware" },
        }),
      },
    });
    await waitFor(() => expect(q("pf-serial-port")).not.toBeNull());
    expect((q("pf-serial-port") as HTMLInputElement).value).toBe("COM7");
    expect((q("pf-serial-baud") as HTMLInputElement).value).toBe("250000");
    expect((q("pf-serial-flow") as HTMLSelectElement).value).toBe("hardware");
  });
});

describe("ProfileDialog 保存与协议交互回归", () => {
  beforeEach(() => { calls.length = 0; saveWait = null; failOwnPassword = false; vaultRecordsResult = []; sharedCredsResult = []; vaultListErr = null; sharedNameResult = null; settingsValues = {}; });
  async function newProfile() {
    render(ProfileDialog, { open: true });
    await fireEvent.input(screen.getByTestId("pf-name"), { target: { value: "测试服务器" } });
    await fireEvent.input(screen.getByTestId("pf-host"), { target: { value: "server.example" } });
    await fireEvent.input(screen.getByTestId("pf-username"), { target: { value: "demo" } });
  }
  it("必填错误定位到字段且不调用保存", async () => {
    render(ProfileDialog, { open: true });
    await fireEvent.click(screen.getByTestId("pf-save"));
    expect(screen.getByRole("alert").textContent).toContain("会话名称");
    expect(document.activeElement).toBe(screen.getByTestId("pf-name"));
    expect(calls.some(c => c.cmd === "profile_save")).toBe(false);
  });
  it("保存进行中不会重复创建", async () => {
    await newProfile();
    let resolve!: () => void;
    saveWait = new Promise<void>(r => { resolve = r; });
    await fireEvent.click(screen.getByTestId("pf-save"));
    await fireEvent.click(screen.getByTestId("pf-save"));
    expect(calls.filter(c => c.cmd === "profile_save")).toHaveLength(1);
    expect((screen.getByTestId("pf-save") as HTMLButtonElement).disabled).toBe(true);
    resolve(); saveWait = null;
    await waitFor(() => expect((screen.getByTestId("pf-save") as HTMLButtonElement).disabled).toBe(false));
  });
  it("密码保存失败后重试更新已创建的连接，不重复新建", async () => {
    await newProfile();
    await fireEvent.click(screen.getByRole("tab", { name: "认证" }));
    await fireEvent.input(screen.getByTestId("pf-own-password"), { target: { value: "dummy-password" } });
    failOwnPassword = true;
    await fireEvent.click(screen.getByTestId("pf-save"));
    expect(screen.getByRole("alert").textContent).toContain("密码未保存");
    failOwnPassword = false;
    await fireEvent.input(screen.getByTestId("pf-own-password"), { target: { value: "dummy-password" } });
    await fireEvent.click(screen.getByTestId("pf-save"));
    const saves = calls.filter(c => c.cmd === "profile_save");
    expect(saves).toHaveLength(2);
    expect(saves[0].args.id).toBeNull();
    expect(saves[1].args.id).toBe("11111111-1111-1111-1111-111111111111");
  });
  it("RDP 只展示适用页签和密码认证", async () => {
    render(ProfileDialog, { open: true, initial: profile({ protocol: "rdp" }) });
    expect(screen.getAllByRole("tab").map(t => t.textContent?.trim())).toEqual(["常规", "认证", "跳板"]);
    await fireEvent.click(screen.getByRole("tab", { name: "认证" }));
    expect(screen.queryByTestId("pf-cred-key")).toBeNull();
    expect(screen.queryByTestId("pf-vault-empty")).toBeNull();
    expect(screen.queryByText("允许交互认证（验证码等）")).toBeNull();
  });
});
