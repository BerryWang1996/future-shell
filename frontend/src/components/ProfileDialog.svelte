<script lang="ts">
  import { untrack, tick } from "svelte";
  import { invoke, settingGet } from "../lib/ipc";
  import { tabNavigation } from "../lib/tabNavigation";
  import { useFocusTrap } from "../lib/focusTrap";
  import { SCHEMES } from "../lib/term-schemes";
  import type { AiPolicy, AuthRef, HostKeyPin, HostKeyPolicy, ImportSummary, JumpHop, Profile, SecretKind, SftpDefaults, TermSettings } from "../lib/types";

  let {
    open = false,
    initial = null,
    initialTab = null,
    onClose = () => {},
    onSaved = (_p: Profile) => {},
  }: {
    open?: boolean;
    initial?: Profile | null;
    /** 打开时直接落在哪一页（2026-09-01 失败面板的「配置认证…」用）。null = 常规页。
     *  每次打开都按它定位（不记上次停在哪）——口径同 SettingsDialog。 */
    initialTab?: "general" | "auth" | "jump" | "hostkey" | "terminal" | "sftp" | "ai" | "env" | null;
    onClose?(): void;
    onSaved?(p: Profile): void;
  } = $props();

  const NIL_UUID = "00000000-0000-0000-0000-000000000000";

  const TABS = [
    { id: "general", label: "常规" },
    { id: "auth", label: "认证" },
    { id: "jump", label: "跳板" },
    { id: "hostkey", label: "主机密钥" },
    { id: "terminal", label: "终端" },
    { id: "sftp", label: "SFTP" },
    { id: "ai", label: "AI" },
    { id: "env", label: "环境变量" },
  ] as const;
  type TabId = (typeof TABS)[number]["id"];
  let tab = $state<TabId>("general");

  // 常规
  let name = $state("");
  let groupPath = $state("");
  let host = $state("");
  let port = $state(22);
  // 协议（RDP 阶段 1）：切换时端口跟随默认（22/3389）。不改后端
  // default_port——那是 SSH 的语义，改它会让存量 profile 端口漂移。
  let protocol = $state<"ssh" | "rdp" | "serial">("ssh");
  // 串口（M7.4）。端口名可选可填：枚举列表是**便利**不是白名单——容器里的 socat 伪终端、
  // Linux 上没被 sysfs 扫到的虚拟口，用户自己打得出来就该能连。
  let serialPort = $state("");
  let serialBaud = $state(115200);
  let serialDataBits = $state("eight");
  let serialParity = $state("none");
  let serialStopBits = $state("one");
  let serialFlow = $state("none");
  let serialPorts = $state<{ name: string; kind: string; description: string | null }[]>([]);
  let serialNote = $state("");
  let serialBauds = $state<number[]>([]);
  /** 串口档案上不成立的页签：没有认证、没有跳板、没有主机密钥、没有 SFTP。
   *  留着它们不是「多几个空页」——用户会在认证页配一遍密码然后奇怪为什么没用上。 */
  const SERIAL_HIDDEN: readonly TabId[] = ["auth", "jump", "hostkey", "sftp", "env"];
  const visibleTabs = $derived(
    TABS.filter((t) => t.id !== "ai" && (protocol === "serial" ? !SERIAL_HIDDEN.includes(t.id) : protocol === "rdp" ? ["general", "auth", "jump"].includes(t.id) : true)),
  );
  let username = $state("");
  // 认证
  // vault 记录 id 是 Rust `AuthRef.vault_record: Option<u64>`——**数字**。历史实现在提交时
  // `String(vaultRecord)` 转成字符串，Rust 侧 u64 反序列化当场失败，整个 profile_save 存不进去。
  let vaultRecord = $state<number | null>(null);
  /** Rust `AuthRef.passphrase_vault_record`：加密私钥的口令记录。与 vaultRecord 分列两栏是硬约束——
   *  那一栏承载「口令 **或** 私钥 PEM」二选一，加密私钥需要**两份**秘密同时在场。 */
  let passphraseRecord = $state<number | null>(null);
  let allowAgent = $state(false);
  let allowKbd = $state(true);
  let kbdAutoSingle = $state(false);
  // 主机密钥：Rust `HostKeyPolicy` 是 snake_case 裸枚举，取值只有这三个（无 `accept_new`）
  let policy = $state<HostKeyPolicy>("tofu");
  let policyTouched = false;
  let backfillVersion = 0;
  let pins = $state<HostKeyPin[]>([]);
  let importToast = $state("");
  // 终端
  let term = $state("");
  let encoding = $state("");
  /** 可选编码清单（值 + 人读名），来自后端。取不到就只留「UTF-8（默认）」那一项——
   *  空下拉比一份前端自己编的清单好：编出来的那份迟早与后端分叉，表现是「选了却报错」。 */
  let encodings = $state<[string, string][]>([]);
  let scrollback = $state("");
  let fontSizeInput = $state("");
  let schemeSel = $state("");
  // SFTP
  let localDir = $state("");
  let remoteDir = $state("");
  let downloadSandbox = $state("");
  // 环境变量
  let envRows = $state<{ key: string; value: string }[]>([]);
  // 表单错误
  let formError = $state("");
  let saving = $state(false);
  let savedId: string | null = null;
  let dialogEl = $state<HTMLDivElement | null>(null);
  $effect(() => {
    if (open && dialogEl) return useFocusTrap(dialogEl, { initial: dialogEl.querySelector<HTMLElement>('[data-testid="pf-name"]') });
  });
  $effect(() => { if (!visibleTabs.some(t => t.id === tab)) tab = "general"; });
  function close() { if (!saving) onClose(); }
  async function invalid(message: string, page: TabId, selector: string) {
    formError = message;
    tab = page;
    await tick();
    dialogEl?.querySelector<HTMLElement>(selector)?.focus();
  }

  /* ── 跳板链（审计2 #23）────────────────────────────────────────────────────
   * 旧对话框对 jump 只有一行 `initial?.jump ?? []` 原样透传：普通用户不能建也不能改，
   * 导入还剥离逐跳凭据与信任断言（repo.rs 的既有不变式，见 test 注释），于是跳板配置
   * 没有任何用户可达的维护入口。这里补上完整编辑面。
   *
   * 行承载 Rust `JumpHop` 的**全部**字段：host/port/username/vaultRecord/allowAgent/policy/pins
   * 有控件；passphraseRecord 与 kbd 两旗无控件、原样透传——不透传即「保存一次，加密私钥
   * 的口令记录和 kbd 设置就静默蒸发」。 */
  type JumpRow = {
    key: number; // 稳定 each 键：行可增删移动，不能拿下标当 key（复用错误行状态）
    host: string;
    port: number; // 运行时可能被清成 null（Svelte number input 空串 → null），保存前校验兜住
    username: string;
    vaultRecord: number | null;
    passphraseRecord: number | null;
    allowAgent: boolean;
    allowKbd: boolean;
    kbdAutoSingle: boolean;
    policy: "" | HostKeyPolicy;
    pins: HostKeyPin[];
  };
  let jumpSeq = 0; // 非响应式计数器：不渲染，只发号
  let jumpRows = $state<JumpRow[]>([]);
  // 逐跳钉选取器（按行 key 定位：行删除/重排后下标会漂，key 不会）
  let pickerKey = $state<number | null>(null);
  let hopPickerKeys = $state<{ key_type: string; key_blob: string; fingerprint_sha256: string; source: string }[]>([]);
  let hopPickerError = $state("");

  function blankJumpRow(): JumpRow {
    return {
      key: jumpSeq++, host: "", port: 22, username: "", vaultRecord: null, passphraseRecord: null,
      allowAgent: false, allowKbd: true, kbdAutoSingle: false, policy: "", pins: [],
    };
  }
  /** 上限与后端 `limits::JUMP_MAX`（8）同源——按钮先行禁用，超限保存后端也会拒。 */
  function addJump() {
    if (jumpRows.length >= 8) return;
    jumpRows = [...jumpRows, blankJumpRow()];
  }
  function removeJump(i: number) {
    const gone = jumpRows[i];
    if (gone && pickerKey === gone.key) pickerKey = null;
    jumpRows = jumpRows.filter((_, idx) => idx !== i);
  }
  function moveJump(i: number, delta: -1 | 1) {
    const j = i + delta;
    if (j < 0 || j >= jumpRows.length) return;
    const next = [...jumpRows];
    [next[i], next[j]] = [next[j], next[i]];
    jumpRows = next;
  }
  async function openHopPinPicker(key: number) {
    hopPickerError = ""; hopPickerKeys = [];
    pickerKey = key;
    const row = jumpRows.find((x) => x.key === key);
    if (!row) return;
    const h = row.host.trim();
    if (!h) { hopPickerError = "请先填写该跳的主机地址"; return; }
    try {
      // 与目标机选取器同一入口：钉只来自信任库（本机确认过的密钥），不开放手填
      hopPickerKeys = await invoke("hostkey_known_keys", { host: h, port: Number(row.port) || 22 });
    } catch (e) {
      hopPickerError = `读取信任库失败：${e}`;
    }
  }
  function closeHopPinPicker() { pickerKey = null; }
  function addHopPin(i: number, k: { key_blob: string; fingerprint_sha256: string }) {
    // 去重按 key_blob（被 decide 逐字比对的那一格），同目标机选取器口径
    jumpRows = jumpRows.map((r, idx) => {
      if (idx !== i || r.pins.some((p) => p.key_blob === k.key_blob)) return r;
      return { ...r, pins: [...r.pins, { key_blob: k.key_blob, fingerprint_sha256: k.fingerprint_sha256 }] };
    });
  }
  function removeHopPin(i: number, keyBlob: string) {
    jumpRows = jumpRows.map((r, idx) =>
      idx === i ? { ...r, pins: r.pins.filter((p) => p.key_blob !== keyBlob) } : r,
    );
  }

  // bind:this 目标必须声明为 $state（Svelte 5 runes）：普通 let 的赋值不进响应图，
  // 「导入 known_hosts…」按钮读到的可能仍是 undefined（non_reactive_update）。
  let fileInput: HTMLInputElement | undefined = $state();

  $effect(() => {
    // 审计2 #29：**关闭时也要清**行内录入的明文秘密。这个对话框挂在 App 顶层、
    // 活到进程结束，而 `newSecret` 承载的是用户刚敲进去的密码或整段私钥 PEM。
    // 旧实现只有 `if (!open) return;`，于是「填了一半按取消」「按 Escape」「保存档案但
    // 没提交凭据」之后，那段明文原样躺在组件状态里，直到下一次打开对话框才被覆盖——
    // 而「下一次」可能永远不来。
    //
    // 这一步做到的是**尽早丢掉引用**，不是清零：JS 字符串不可变，没有办法把它从堆上擦掉。
    // 真正的清零发生在 Rust 侧（vault_put_secret 入口的 Zeroizing）。诚实地说清边界，
    // 好过在注释里写「已清除明文」。
    clearVaultForm();
    if (!open) return;
    void backfill();
    // 编码清单（M7.4）：拿不到就留一项 UTF-8，不阻断整个对话框——用户来这儿多半
    // 是改别的字段的。
    void invoke<[string, string][]>("term_encodings")
      .then((l) => (encodings = Array.isArray(l) ? l : []))
      .catch(() => (encodings = []));
  });

  /**
   * 串口列表与常用波特率。
   *
   * 枚举失败不弹错误框：拿不到列表不代表不能用串口（容器里的 socat 伪终端、Linux 上
   * 没被 sysfs 扫到的口都不会出现在列表里），用户手打端口名照样能连。所以后端把失败
   * 降级成「空列表 + 一行说明」，这里原样呈现那行说明。
   */
  async function loadSerialPorts(): Promise<void> {
    try {
      const r = await invoke<{ ports: typeof serialPorts; note: string }>("serial_list_ports");
      serialPorts = r.ports ?? [];
      serialNote = r.note ?? "";
    } catch (e) {
      serialPorts = [];
      serialNote = `列不出串口（${e}）。可以直接填端口名。`;
    }
    try {
      serialBauds = await invoke<number[]>("serial_common_bauds");
    } catch {
      serialBauds = [];
    }
  }

  /** 丢掉行内录入的明文与其残留状态。见上方 $effect 与「存入 Vault…」折叠按钮。 */
  function clearVaultForm() {
    newSecret = "";
    newLabel = "";
    vaultFormError = "";
    vaultFormOpen = false;
  }

  async function backfill() {
    const version = ++backfillVersion;
    policyTouched = false;
    // 在 backfill **内**定位而不是另起 $effect：另起的会被本函数的同步重置踩掉。
    tab = initialTab ?? "general";
    importToast = "";
    formError = "";
    const p = initial;
    savedId = p?.id ?? null;
    name = p?.name ?? "";
    groupPath = p?.group_path ?? "";
    host = p?.host ?? "";
    port = p?.port ?? 22;
    protocol = p?.protocol ?? "ssh";
    serialPort = p?.serial?.port ?? "";
    serialBaud = p?.serial?.baud ?? 115200;
    serialDataBits = p?.serial?.data_bits ?? "eight";
    serialParity = p?.serial?.parity ?? "none";
    serialStopBits = p?.serial?.stop_bits ?? "one";
    serialFlow = p?.serial?.flow ?? "none";
    // 判据读的是**参数 `p`**，不是 `protocol` 这个 $state。
    //
    // 这一段跑在 backfill 的同步前半截，而 backfill 由上面那个 $effect 调用——读一个 $state
    // 就等于给那个 effect 加一条依赖。读 `protocol` 的后果是「一改协议就重跑 backfill」，
    // 而 backfill 的第一件事就是把 protocol 设回档案里的值：协议下拉看起来**完全切不动**
    //（实测如此，不是推理）。切换那一路的加载在下拉的 onchange 里，与这里各管各的。
    if ((p?.protocol ?? "ssh") === "serial") void loadSerialPorts();
    username = p?.username ?? "";
    vaultRecord = p?.auth?.vault_record ?? null;
    passphraseRecord = p?.auth?.passphrase_vault_record ?? null;
    allowAgent = p?.auth?.allow_agent ?? false;
    allowKbd = p?.auth?.allow_kbd_interactive ?? true;
    kbdAutoSingle = p?.auth?.kbd_auto_answer_single ?? false;
    // 档案自身的策略是裸字符串（不再是 `{ mode }` 包装）；新建（initial=null）才回退全局默认（R31 消费方）
    policy = p?.host_key_policy ?? "tofu";
    pins = p?.host_key_pins ?? [];
    term = p?.term?.term ?? "";
    encoding = p?.term?.encoding ?? "";
    scrollback = p?.term?.scrollback_lines != null ? String(p.term.scrollback_lines) : "";
    fontSizeInput = p?.term?.theme_override?.font_size != null ? String(p.term.theme_override.font_size) : "";
    schemeSel = p?.term?.theme_override?.scheme ?? "";
    localDir = p?.sftp?.local_dir ?? "";
    remoteDir = p?.sftp?.remote_dir ?? "";
    downloadSandbox = p?.sftp?.download_sandbox ?? "";
    envRows = Object.entries(p?.env ?? {}).map(([key, value]) => ({ key, value }));
    // 跳板逐跳回填。`?.` 防御缺字段的旧档案（auth 缺省字段、缺失 host_key_pins 等）；
    // 无 UI 的字段随行透传，保存时原样回去。
    jumpRows = (p?.jump ?? []).map((h) => ({
      key: jumpSeq++,
      host: h.host,
      port: h.port,
      username: h.username,
      vaultRecord: h.auth?.vault_record ?? null,
      passphraseRecord: h.auth?.passphrase_vault_record ?? null,
      allowAgent: h.auth?.allow_agent ?? false,
      allowKbd: h.auth?.allow_kbd_interactive ?? false,
      kbdAutoSingle: h.auth?.kbd_auto_answer_single ?? false,
      policy: h.host_key_policy ?? "",
      pins: h.host_key_pins ?? [],
    }));
    pickerKey = null;
    hopPickerKeys = [];
    hopPickerError = "";
    if (!p) {
      const defaultPolicy = await settingGet<HostKeyPolicy>("hostkey.defaultPolicy", "tofu");
      if (version === backfillVersion && !policyTouched) policy = defaultPolicy;
    }
  }

  /** Rust `AiPolicy` 的默认值（`AutoExec::Off` + 不放行 MCP）。
   *  `auto_execute` 是**三态字符串**枚举，不是 boolean——旧实现发 `false`，Rust 侧当场反序列化失败。 */
  function blankAiPolicy(): AiPolicy { return { auto_execute: "off", mcp_allowed: false }; }

  async function save() {
    if (saving) return;
    formError = "";
    if (Array.from(name.trim()).length > 128) return invalid("会话名称最多 128 个字符。", "general", '[data-testid="pf-name"]');
    if (!name.trim()) return invalid("请填写会话名称，便于在列表中识别。", "general", '[data-testid="pf-name"]');
    if (protocol === "serial") {
      if (!serialPort.trim()) return invalid("请选择或填写串口，例如 COM3。", "general", '[data-testid="pf-serial-port"]');
      if (!Number.isInteger(Number(serialBaud)) || Number(serialBaud) <= 0) return invalid("波特率必须为正整数。", "general", '[data-testid="pf-serial-baud"]');
    } else {
      if (!username.trim()) return invalid("请填写登录用户名。", "general", '[data-testid="pf-username"]');
      if (!host.trim()) return invalid("请填写主机名或 IP 地址。", "general", '[data-testid="pf-host"]');
      if (!Number.isInteger(port) || port < 1 || port > 65535) return invalid("端口必须为 1–65535 的整数。", "general", '[data-testid="pf-port"]');
    }
    if (protocol !== "rdp" && fontSizeInput != null && String(fontSizeInput).trim() !== "" && (!Number.isInteger(Number(fontSizeInput)) || Number(fontSizeInput) < 8 || Number(fontSizeInput) > 32)) return invalid("字号须在 8–32 之间，留空则跟随全局。", "terminal", '[data-testid="pf-font-size"]');
    if (protocol !== "rdp" && scrollback.trim() && (!Number.isInteger(Number(scrollback)) || Number(scrollback) < 0 || Number(scrollback) > 1_000_000)) return invalid("滚动行数须为 0–1000000 的整数。", "terminal", '[data-testid="pf-scrollback"]');
    // 跳板链逐行校验（审计2 #23）：规则与后端 `Profile::validate` 同源（第 N 跳主机非空/≤253、
    // 端口非 0、用户名 ≤128、总数 ≤8）——这里提前到行号粒度，且 FE 不发明更严的规则
    // （跳板用户名允许为空，与后端一致；端口 0 与空值一并拦下）。长度按码点计，与后端
    // chars().count() 同口径。
    if (jumpRows.length > 8) { formError = "跳板数量超过上限 8"; return; }
    for (const [i, r] of jumpRows.entries()) {
      const n = i + 1;
      if (!r.host.trim()) { formError = `跳板第 ${n} 行：主机不能为空`; return; }
      if (Array.from(r.host.trim()).length > 253) { formError = `跳板第 ${n} 行：主机长度超过上限 253`; return; }
      if (!Number.isInteger(r.port) || r.port < 1 || r.port > 65535) { formError = `跳板第 ${n} 行：端口必须为 1–65535 的数字`; return; }
      if (Array.from(r.username).length > 128) { formError = `跳板第 ${n} 行：用户名长度超过上限 128`; return; }
    }
    const jump: JumpHop[] = jumpRows.map((r) => ({
      host: r.host.trim(),
      port: r.port,
      username: r.username.trim(),
      auth: {
        vault_record: r.vaultRecord,
        passphrase_vault_record: r.passphraseRecord,
        allow_agent: r.allowAgent,
        allow_kbd_interactive: r.allowKbd,
        kbd_auto_answer_single: r.kbdAutoSingle,
      },
      host_key_policy: r.policy === "" ? null : r.policy,
      host_key_pins: r.pins,
    }));
    const env: Record<string, string> = {};
    for (const r of envRows) if (r.key.trim()) env[r.key.trim()] = r.value;
    // TermSettings 的每一栏都是 Rust `Option<T>`：留空提交 null（＝未设置，由终端层取默认），
    // 不要塞 80/24/1000 这类前端编造的「默认值」——那会把「用户没配」伪装成「用户配成了这个」。
    const termSettings: TermSettings = {
      term: term.trim() || null,
      encoding: encoding.trim() || null,
      scrollback_lines: scrollback.trim() ? Number(scrollback) : null,
      theme_override: (schemeSel || fontSizeInput)
        ? { scheme: schemeSel || null, font_size: fontSizeInput ? Number(fontSizeInput) : null }
        : null,
    };
    const sftp: SftpDefaults = {
      local_dir: localDir.trim() || null,
      remote_dir: remoteDir.trim() || null,
      download_sandbox: downloadSandbox.trim() || null,
    };
    const auth: AuthRef = {
      vault_record: vaultRecord,               // Option<u64>：直接发数字/null，不做 String() 转换
      passphrase_vault_record: passphraseRecord,
      allow_agent: allowAgent,
      allow_kbd_interactive: allowKbd,
      kbd_auto_answer_single: kbdAutoSingle,
    };
    const profile: Profile = {
      id: savedId ?? NIL_UUID,
      name: name.trim(),
      group_path: groupPath.trim() || null,
      host: host.trim(),
      port,
      protocol,
      serial: {
        port: serialPort.trim(),
        baud: Number(serialBaud) || 115200,
        data_bits: serialDataBits,
        parity: serialParity,
        stop_bits: serialStopBits,
        flow: serialFlow,
      },
      username: username.trim(),
      auth,
      // Rust 是 `Vec<JumpHop>`：无跳板发空数组。发 null 会失败——`#[serde(default)]` 只兜**缺键**，
      // 显式 null 仍要走 Vec 的反序列化。审计2 #23 起由跳板页逐行构建（见 save 开头），不再是透传。
      jump,
      host_key_policy: policy,                 // 裸字符串，不是 `{ mode }`
      host_key_pins: pins,
      env,
      term: termSettings,
      sftp,
      // AI 页在 MVP 禁用：不发明取值，原样透传既有策略，避免「打开属性页点保存」把用户的设置抹成默认
      ai_policy: initial?.ai_policy ?? blankAiPolicy(),
    };
    saving = true;
    try {
      // `id` 必须与 profile 并列显式提交：Rust `profile_save(id: Option<String>, ..)` 用它覆盖
      // `profile.id`，`None` 即视为新建并生成新 UUID。历史实现漏发此参，于是**每次编辑既有档案
      // 都会另存成一条新记录**（旧记录仍在，侧栏出现重影）。新建时发 null 才是「请生成 id」。
      const id = await invoke<string>("profile_save", { id: savedId, profile });
      savedId = id;
      // 专属密码在 profile 落库**之后**才写：新建连接时 id 是这一步才有的，
      // 而 profile_set_own_password 要靠 id 找到档案回写 vault_record。
      // 留空 = 不修改（已有的保持原样、没有的就让连接时弹框问）——
      // 这与占位文案说的是同一件事。
      if (credMode === "own" && ownPassword !== "") {
        const b64 = btoa(String.fromCharCode(...new TextEncoder().encode(ownPassword)));
        ownPassword = ""; // 明文当场丢掉（同 clearVaultForm 的口径，spec §3.2）
        try {
          await invoke("profile_set_own_password", { profileId: id, secretB64: b64 });
        } catch (e) {
          // 档案已经存好了，只是密码没存上：如实说，别让用户以为整个保存失败了
          formError = `连接已保存，但密码未保存。请重新填写密码后重试（不会重复新建连接）：${e}`;
          tab = "auth";
          return;
        }
      }
      onSaved({ ...profile, id });
      onClose();
    } catch (e) {
      formError = `保存失败：${e}`;
    } finally {
      saving = false;
    }
  }

  async function onImportKnownHosts(e: Event) {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (!file) return;
    // 审计2 #37 纵深防御：Rust 侧 import_known_hosts 持同一上限
    // （fs_connmgr::model::limits::IMPORT_MAX_BYTES），这里先挡一次，
    // 免得超大文件整份读进内存、推过 IPC 再被后端拒。
    if (file.size > 8 * 1024 * 1024) {
      importToast = "导入失败：文件超过 8 MiB 上限";
      return;
    }
    const content = await file.text();
    try {
      const r = await invoke<ImportSummary>("hostkey_import", { content });
      // 「冲突 N」曾是一个不作解释的裸数字（审计 P2）：用户看不出发生了什么、要不要担心，
      // 而它恰恰意味着某台主机的一把密钥没进来。后端已按协商顺序择优保留，
      // 剩下的冲突只可能是「同算法异密钥」或「本机已亲手确认过另一把」——两种都值得说清楚。
      //
      // 审计2 #22：曾经的「跳过 N」把「本来就在库里」和「整行安全信息被丢掉」并成一个数字。
      // 现在按原因分列，且**未导入的那几类必须明说没有生效**——用户据此决定要不要另想办法，
      // 而一个笼统的「跳过」只会让他以为一切正常。
      importToast =
        `导入 ${r.imported}` +
        (r.upgraded ? ` / 择优替换 ${r.upgraded}` : "") +
        (r.revoked ? ` / 已记为吊销 ${r.revoked}（此后硬拒绝，不可点击接受）` : "") +
        (r.duplicate ? ` / 已在库中 ${r.duplicate}` : "") +
        (r.conflicted
          ? ` / 冲突 ${r.conflicted}（本机每台主机只保留一条密钥记录，这些行与已有记录不一致，未导入）`
          : "") +
        (r.cert_authority
          ? ` / 证书颁发机构 ${r.cert_authority} 行未导入（本版本不验证 SSH 证书）`
          : "") +
        (r.hashed
          ? ` / 哈希主机名 ${r.hashed} 行未导入（哈希不可逆，无法还原主机名）`
          : "") +
        (r.pattern
          ? ` / 通配/取反主机 ${r.pattern} 行未导入（本版本按主机名精确匹配）`
          : "") +
        (r.malformed ? ` / 格式无法解析 ${r.malformed} 行` : "");
    } catch (err) {
      importToast = `导入失败：${err}`;
    }
  }

  /* ── 凭据归属（2026-08-28）──────────────────────────────────────────────
   *
   * 用户要的模型：**默认每个连接一份自己的凭据，想共享才主动抽出来**。
   * 原实现只有一个「从全局记录池里挑一条」的下拉框，那个形状在诱导共享。
   *
   * 两个单选显式切换，默认停在「本连接专用」：
   * · own    —— 就地输密码，保存时存成只归本连接的 Vault 记录；
   * · shared —— 从**已被显式共享**的凭据里挑一条（不是全部记录）。
   *
   * 停在哪一档由现状推导：已绑的记录是私钥类 → key；在共享登记表里 → shared；否则 own。
   *
   * · key（2026-09-02 加）—— 从保险库里挑一条**私钥**记录。私钥天然是一把配多台机器的，
   *   不属于 2026-08-28 那条「密码不诱导共享」的裁定范围；此前它没有入口：共享列表只有
   *   显式共享过的记录，于是用户把两把私钥存进保险库之后，唯一能看见它们的地方是
   *   「私钥口令」下拉——置灰、写着「不能作口令」——他点了两天，报了三次「下拉选不了」。
   */
  type CredMode = "own" | "shared" | "key";
  let credMode = $state<CredMode>("own");
  $effect(() => { if (protocol === "rdp" && credMode === "key") credMode = "shared"; });
  /** 用户在本次打开期间碰过单选没有。判档是异步的（要等凭据列表落地），等它落地时若用户已经
   *  自己选了一档，就不能再把他的选择改回去——首版正是这样把刚点的「私钥」档拍回「密码」档。 */
  let credModeTouched = false;
  /** 就地输入的专属密码。**保存后立刻清空**（明文不留在组件状态里）。 */
  let ownPassword = $state("");
  /** 本连接已经存过专属密码（决定输入框的占位文案是「已保存」还是「留空则询问」）。 */
  let hasOwnPassword = $state(false);
  /** 当前绑定的记录若是共享的，这里是它的名字（也用来判定单选停在哪档）。 */
  let sharedName = $state<string | null>(null);
  /** 可选的共享凭据（**不是**全部 Vault 记录）。 */
  let sharedCreds = $state<{ recordId: number; name: string; kind: string }[]>([]);

  async function refreshSharedCreds() {
    try {
      const rows =
        await invoke<{ record_id: number; name: string; kind: string | null }[]>(
          "credentials_shared_list",
        );
      // kind 为 null（Vault 未解锁，读不到类别）时按未知处理——与本组件对
      // 未知类别的既有口径一致：fail-closed，置灰而不是当成可用。
      sharedCreds = rows.map((r) => ({
        recordId: r.record_id,
        name: r.name,
        kind: r.kind ?? "unknown",
      }));
    } catch {
      sharedCreds = [];
    }
  }

  /** 打开对话框时按现状决定单选停在哪一档。 */
  async function resolveCredMode() {
    // 每一处对 credMode 的写都要过 credModeTouched：本函数会被调两次（打开时同步一次、列表落地后一次），
    // 第二次可能在用户已经点了某一档之后才落定——那时只更新事实（sharedName / hasOwnPassword），不改他的选择。
    if (vaultRecord === null) {
      hasOwnPassword = false;
      sharedName = null;
      if (!credModeTouched) credMode = "own";
      return;
    }
    // 绑的是私钥 → 私钥档（vaultRecords 须先取到；取不到——比如库锁着——类别未知，按下面的旧口径走）
    if (vaultRecords.find((r) => r.id === vaultRecord)?.kind === "private_key") {
      hasOwnPassword = false;
      sharedName = null;
      if (!credModeTouched) credMode = "key";
      return;
    }
    hasOwnPassword = true;
    let name: string | null = null;
    try {
      name = await invoke<string | null>("credential_is_shared", { recordId: vaultRecord });
    } catch {
      name = null;
    }
    sharedName = name; // 不再在函数开头清空：清空到落定之间「取消共享」按钮会闪一下
    if (!credModeTouched) credMode = name === null ? "own" : "shared";
  }

  /**
   * 取消共享。**不删 Vault 记录**——本连接还在用它。
   *
   * 取消之后它从其他连接的选择器里消失，但**已经引用它的连接保持原样**
   * （那些连接引用的是记录 id，不是共享登记）。这是刻意的：取消共享不该
   * 把别人正在用的连接弄坏，那是「删除凭据」才该有的后果。
   */
  async function unshareCurrent() {
    if (vaultRecord === null) return;
    try {
      await invoke("credential_unshare", { recordId: vaultRecord });
      sharedName = null;
      await refreshSharedCreds();
    } catch (e) {
      formError = `取消共享失败：${e}`;
    }
  }

  /** 把当前这条专属凭据抽出来共享（要起名字——那是它在别处的唯一身份）。 */
  async function shareCurrent() {
    const name = window.prompt(
      "给这条共享凭据起个名字（其他连接的选择器里只显示这个名字）",
      `${username}@${host}`,
    );
    if (name === null) return; // 用户取消
    if (vaultRecord === null) return;
    try {
      await invoke("credential_share", { recordId: vaultRecord, name });
      sharedName = name.trim();
      await refreshSharedCreds();
    } catch (e) {
      formError = `设为共享失败：${e}`;
    }
  }

  // Vault 凭据选取器 + 行内录入
  let vaultRecords = $state<{ id: number; kind: string; label: string }[]>([]);
  let vaultFormOpen = $state(false);
  let newKind = $state<Extract<SecretKind, "password" | "private_key">>("password");
  let newLabel = $state("");
  let newSecret = $state("");
  let vaultFormError = $state("");

  /** 凭据列表取不到时的原因（null = 没有错）。**不静默吞**：见下方注释。 */
  let vaultListError = $state<string | null>(null);
  /** 取过一次没有（用于区分「还没取」与「真的一条都没有」）。 */
  let vaultListLoaded = $state(false);

  async function refreshVaultRecords() {
    try {
      vaultRecords = await invoke<{ id: number; kind: string; label: string }[]>("vault_list_secrets");
      vaultListError = null;
    } catch (e) {
      // 2026-09-01：此前这里是 `catch { vaultRecords = [] }`——**静默吞掉**。
      // 后果就是用户两次报的那件事：取不到记录 → 空列表 → 下拉里只剩「无」→
      // 看起来就是「这个下拉选不了」，而界面一个字都不说，用户无从知道
      // 是「我还没存过凭据」还是「保险库锁着」还是「后端出错了」。
      vaultRecords = [];
      const raw = String(e);
      vaultListError = raw.includes("vault 未解锁")
        ? "保险库未解锁，读不到已存的凭据——请先在状态栏点锁图标解锁，再回来选。"
        : `读取凭据列表失败：${raw}`;
    } finally {
      vaultListLoaded = true;
    }
  }
  $effect(() => {
    if (!open) return;
    // untrack：本 effect 只该依赖 open。resolveCredMode 会同步读 vaultRecords / vaultRecord，
    // 不隔离的话 refreshVaultRecords 每次赋新数组都触发本 effect 重跑 → 再 refresh → 无限循环
    //（首版正是这样把 vitest 的 worker 跑挂的）。
    untrack(() => {
      credModeTouched = false;
      // 先按手头的信息同步判一次（没绑记录 → 密码档，立刻就对；绑了的要看类别）……
      void resolveCredMode();
      // ……再等凭据列表落地后复判一次——「绑的是不是私钥」只有拿到类别才知道。
      // 用户若在这期间已经自己点了某一档，以他的为准，不再改。
      void (async () => {
        await refreshVaultRecords();
        await refreshSharedCreds();
        if (!credModeTouched) await resolveCredMode();
      })();
    });
  });

  /* ── 凭据类别：显示名与可用性（审计2 #20 的前端一半）─────────────────────────────
   * 后端现在按记录**自己声明的** SecretKind 分流，类别不匹配一律硬错误、不再静默降级。
   * 于是两个选取器必须在**选之前**就说清哪些记录用不了——否则用户只能在连接时收到一句
   * 「已拒绝」，而那时他既看不到记录类别，也不知道该改哪一栏。
   *
   * `vaultRecords[].kind` 仍标注为 string 而不是 SecretKind：它是从 IPC 回来的原始串，
   * 断言它已经是联合成员，等于把「后端出现了前端不认识的类别」这件事从可检测变成不可见。
   * 下面三个函数都对未知串取保守解释（原样显示、判为不可用）。 */
  const KIND_LABEL: Record<SecretKind, string> = {
    password: "密码",
    private_key: "私钥",
    api_key: "API Key",
  };
  /** 未知类别原样回显，绝不回落成某个具体用途——旧写法是 `r.kind === "private_key" ? … : "密码"`，
   *  把每一个不认识的串都显示成「密码」，正是这次要消灭的那种猜测。 */
  function kindLabel(k: string): string {
    return Object.prototype.hasOwnProperty.call(KIND_LABEL, k) ? KIND_LABEL[k as SecretKind] : k;
  }
  /** 能作为 SSH 主凭据的类别：口令与私钥。api_key 会被 `build_credentials` 硬拒。 */
  function usableAsCredential(k: string): boolean {
    return k === "password" || (protocol === "ssh" && k === "private_key");
  }
  /** 私钥口令只能是 password 类别：`load_passphrase` 对其他类别硬拒（拿私钥去解私钥没有意义）。 */
  function usableAsPassphrase(k: string): boolean {
    return k === "password";
  }
  const selectedCredKind = $derived(vaultRecords.find((r) => r.id === vaultRecord)?.kind);
  const selectedPassKind = $derived(vaultRecords.find((r) => r.id === passphraseRecord)?.kind);
  /** 私钥档的候选：只列私钥。 */
  const keyCandidates = $derived(vaultRecords.filter((r) => r.kind === "private_key"));
  /** 私钥口令的候选：只列能作口令的类别。不再把私钥/API Key 置灰摆在里面——
   *  一个「看得见、点不动、说不清」的选项就是诱饵，用户会以为控件坏了（2026-09-02 第三次报同一件事）。 */
  const passphraseCandidates = $derived(vaultRecords.filter((r) => usableAsPassphrase(r.kind)));
  /** 私钥口令只对私钥有意义：主凭据不是私钥时整栏不出现，密码档的用户不该看见它。 */
  const passphraseRelevant = $derived(credMode === "key" || selectedCredKind === "private_key");

  function utf8ToB64(s: string): string {
    return btoa(new TextEncoder().encode(s).reduce((a, b) => a + String.fromCharCode(b), ""));
  }

  async function putSecret() {
    vaultFormError = "";
    if (!newLabel.trim() || !newSecret) { vaultFormError = "标签与内容必填"; return; }
    try {
      const id = await invoke<number>("vault_put_secret", {
        kind: newKind, label: newLabel.trim(), secretB64: utf8ToB64(newSecret),
      });
      const savedKind = newKind;
      newSecret = ""; newLabel = ""; vaultFormOpen = false;
      await refreshVaultRecords();
      vaultRecord = id;
      // 存进来的是私钥 → 直接切到私钥档，让「刚存的那条已被选中」看得见（否则它落在密码档的
      // 「已保存（留空 = 不修改）」占位后面，像是存了一条密码）。
      if (savedKind === "private_key") credMode = "key";
    } catch (e) {
      vaultFormError = `保存失败：${e}`;
    }
  }

  function removePin(i: number) { pins = pins.filter((_, idx) => idx !== i); }

  /* ── 添加钉扎指纹（审计 P1 的可达性那一半）────────────────────────────────────
   * 策略下拉里一直有「指纹钉扎」，但此前全文件只有 removePin，没有任何添加入口：
   * 「导入 known_hosts」写的是**全局信任库**、不写 profile 钉，而 import_json 还会把钉清空。
   * 于是选了钉扎的用户 pins 恒为 []，Rust 侧 `hostkey::decide` 拿到空白名单——修复后是恒硬拒。
   * 只修判定不补入口，等于把这个选项从「假安全」改成「连不上」，两半必须同时落地。
   *
   * 候选**只来自信任库**（hostkey_known_keys），不做手工填写：钉是「本机亲眼见过并确认过
   * 这把公钥」的断言，信任库里的每一行恰好都有这个来源（TOFU 首触时用户点过确认，或用户
   * 自己导入的 known_hosts）。允许手填等于让一个从聊天记录里粘来的指纹冒充本机认知。 */
  let knownKeys = $state<{ key_type: string; key_blob: string; fingerprint_sha256: string; source: string }[]>([]);
  let pinPickerOpen = $state(false);
  let pinPickerError = $state("");

  async function openPinPicker() {
    pinPickerError = ""; knownKeys = [];
    const h = host.trim();
    if (!h) { pinPickerError = "请先填写主机地址"; pinPickerOpen = true; return; }
    try {
      // port 经 <input> 绑定可能是字符串；Rust 侧 u16 反序列化对字符串会直接失败
      knownKeys = await invoke("hostkey_known_keys", { host: h, port: Number(port) || 22 });
    } catch (e) {
      pinPickerError = `读取信任库失败：${e}`;
    }
    pinPickerOpen = true;
  }

  function addPin(k: { key_blob: string; fingerprint_sha256: string }) {
    // 去重按 key_blob（公钥本身）而非指纹：指纹是 blob 的摘要，blob 才是被 decide 逐字比对的那一格
    if (pins.some((p) => p.key_blob === k.key_blob)) return;
    pins = [...pins, { key_blob: k.key_blob, fingerprint_sha256: k.fingerprint_sha256 }];
  }
  function addEnvRow() { envRows = [...envRows, { key: "", value: "" }]; }
</script>

{#if open}
  <div class="overlay" role="presentation">
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <!-- tabindex="-1"：role="dialog" 属交互角色须可聚焦，取 -1 只允许脚本聚焦、不进 Tab 序列 -->
    <div bind:this={dialogEl} class="dialog" role="dialog" aria-modal="true" aria-label={initial ? "编辑会话" : "新建会话"} tabindex="-1"
         data-testid="profile-dialog" onclick={(e) => e.stopPropagation()}
         onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); close(); } }}>
      <header>
        <h2>{initial ? "编辑会话" : "新建会话"}</h2>
        <button class="x" disabled={saving} aria-label="关闭" onclick={close}>×</button>
      </header>
      <div class="body" inert={saving}>
        <!-- 用 div 而非 nav 承载 tablist：nav 是导航地标（noninteractive landmark），
             覆写成 tablist 这类交互角色会同时丢掉地标语义并触发 a11y 告警 -->
        <div class="tabs" role="tablist" aria-label="设置分类" aria-orientation="vertical" use:tabNavigation>
          {#each visibleTabs as t (t.id)}
            <button role="tab" aria-selected={tab === t.id}
                    onclick={() => (tab = t.id)}>{t.label}</button>
          {/each}
        </div>
        <section class="pane">
          {#if tab === "general"}
            <label>名称（必填） <input bind:value={name} data-testid="pf-name" /></label>
            <label>分组（可选） <input bind:value={groupPath} placeholder="工作/生产" /></label>
            <label>协议
              <select bind:value={protocol} data-testid="pf-protocol"
                      onchange={() => { port = protocol === "rdp" ? 3389 : 22; if (protocol === "serial") void loadSerialPorts(); }}>
                <option value="ssh">SSH</option>
                <option value="rdp">RDP（远程 Windows）</option>
                <option value="serial">串口（UART 直连）</option>
              </select>
            </label>
            {#if protocol === "serial"}
              <!-- 串口没有主机/端口/用户名：连接目标是本机的一个设备。把那三栏留在这儿
                   会让用户以为要填点什么，而填什么都不影响它连到哪里。 -->
              <label>串口
                <input list="pf-serial-ports" bind:value={serialPort} data-testid="pf-serial-port"
                       placeholder="COM3 或 /dev/ttyUSB0" />
              </label>
              <datalist id="pf-serial-ports">
                {#each serialPorts as sp (sp.name)}
                  <option value={sp.name}>{sp.description ?? sp.name}</option>
                {/each}
              </datalist>
              <p class="hint" data-testid="pf-serial-hint">
                {serialNote || (serialPorts.length ? `检测到 ${serialPorts.length} 个串口，也可以直接填端口名` : "没检测到串口——直接填端口名也可以")}
              </p>
              <label>波特率
                <input list="pf-serial-bauds" type="number" bind:value={serialBaud} data-testid="pf-serial-baud" />
              </label>
              <datalist id="pf-serial-bauds">
                {#each serialBauds as b (b)}<option value={b}></option>{/each}
              </datalist>
              <label>数据位 <select bind:value={serialDataBits} data-testid="pf-serial-databits">
                <option value="eight">8</option><option value="seven">7</option>
                <option value="six">6</option><option value="five">5</option>
              </select></label>
              <label>校验 <select bind:value={serialParity} data-testid="pf-serial-parity">
                <option value="none">无</option><option value="odd">奇</option><option value="even">偶</option>
              </select></label>
              <label>停止位 <select bind:value={serialStopBits} data-testid="pf-serial-stopbits">
                <option value="one">1</option><option value="two">2</option>
              </select></label>
              <label>流控 <select bind:value={serialFlow} data-testid="pf-serial-flow">
                <option value="none">无</option>
                <option value="software">XON/XOFF（软件）</option>
                <option value="hardware">RTS/CTS（硬件）</option>
              </select></label>
              <p class="hint">
                裸板常常只接 TX/RX/GND 三根线——那种情况下选硬件流控会一个字节都发不出去
                （一直在等 CTS），而现象只是「连上了却什么都没有」。
              </p>
            {:else}
              <label>主机 <input bind:value={host} data-testid="pf-host" /></label>
              <label>端口 <input type="number" bind:value={port} min="1" max="65535" data-testid="pf-port" /></label>
              <label>用户名（必填） <input bind:value={username} data-testid="pf-username" placeholder={protocol === "rdp" ? "用户名 或 域名\\用户名" : "例如 root 或 ubuntu"} /></label>
            {/if}
          {:else if tab === "auth"}
            <!-- 凭据归属（2026-08-28）：默认每个连接一份自己的，共享是显式动作。
                 原来这里只有一个「从全局记录池挑一条」的下拉框，那在诱导共享。 -->
            <fieldset class="cred-mode">
              <legend>凭据</legend>
              <label class="radio">
                <input type="radio" bind:group={credMode} value="own" data-testid="pf-cred-own" onchange={() => (credModeTouched = true)} />
                本连接专用密码
              </label>
              <label class="radio">
                <input type="radio" bind:group={credMode} value="shared" data-testid="pf-cred-shared" onchange={() => (credModeTouched = true)} />
                使用共享凭据
              </label>
              {#if protocol === "ssh"}
              <label class="radio">
                <input type="radio" bind:group={credMode} value="key" data-testid="pf-cred-key" onchange={() => (credModeTouched = true)} />
                使用保险库中的私钥（服务器要求密钥认证时选择）
              </label>
              {/if}
            </fieldset>

            {#if credMode === "own"}
              <label>密码
                <input
                  type="password"
                  bind:value={ownPassword}
                  data-testid="pf-own-password"
                  autocomplete="off"
                  placeholder={hasOwnPassword ? "已保存（留空 = 不修改）" : "留空 = 连接时询问"} />
              </label>
              <!-- 「设为共享」只在已经有一条专属记录时才有意义：
                   还没保存过密码的连接没有可抽出来的东西。 -->
              {#if vaultRecord !== null}
                {#if sharedName}
                  <span class="hint" data-testid="pf-cred-shared-as">已共享为「{sharedName}」</span>
                  <button type="button" class="import" onclick={unshareCurrent} data-testid="pf-cred-unshare"
                    >取消共享</button>
                {:else}
                  <button type="button" class="import" onclick={shareCurrent} data-testid="pf-cred-share"
                    >设为共享…</button>
                {/if}
              {/if}
            {:else if credMode === "key"}
              <label>私钥记录
                <select bind:value={vaultRecord} data-testid="pf-key-select">
                  <option value={null}>无</option>
                  {#each keyCandidates as r (r.id)}
                    <option value={r.id}>#{r.id} · {r.label}</option>
                  {/each}
                </select>
              </label>
              {#if !vaultListError && vaultListLoaded && keyCandidates.length === 0}
                <span class="hint" data-testid="pf-no-key"
                  >保险库里还没有私钥记录。用下方「存入 Vault…」选「私钥」类型、粘贴私钥文本存一条，存完会自动选中。</span>
              {/if}
            {:else}
              <label>共享凭据
                <select bind:value={vaultRecord} data-testid="pf-vault-select">
                  <option value={null}>无</option>
                  {#each sharedCreds as c (c.recordId)}
                    <!-- 类别置灰与主凭据选择器同一套判定（usableAsCredential）：
                         选一条 API Key 当主凭据，后端连接时会直接拒绝，
                         而那时用户看不到类别、也不知道该改哪一栏。 -->
                    <option value={c.recordId} disabled={!usableAsCredential(c.kind)}
                      >{c.name} · {kindLabel(c.kind)}{usableAsCredential(c.kind)
                        ? ""
                        : `（不能用于 ${protocol.toUpperCase()} 认证）`}</option>
                  {/each}
                </select>
              </label>
              {#if sharedCreds.length === 0}
                <span class="hint" data-testid="pf-no-shared"
                  >还没有共享凭据。先在某个连接里存一条专属密码，再点它的「设为共享…」。</span>
              {/if}
            {/if}
            <!-- 已存的 profile 可能指向一条现在用不了的记录（旧版本存的、或该记录后来被改了类别）。
                 选项 disabled 只挡住新的选择，挡不住已经存在的那一条，故仍需一句显式警告。 -->
            {#if selectedCredKind !== undefined && !usableAsCredential(selectedCredKind)}
              <span class="err" role="alert" data-testid="pf-cred-kind-warn"
                >这条记录的类别是「{kindLabel(selectedCredKind)}」，不能用于当前协议的认证。请改选密码记录；SSH 也支持私钥记录。</span
              >
            {/if}
            <!-- 私钥口令单列一栏（Rust AuthRef.passphrase_vault_record）：上一栏承载「口令 或 私钥 PEM」
                 二选一，加密私钥要两份秘密同时在场，合流就永远连不上加密私钥的主机。
                 这一栏此前连类别都不显示，于是把一条私钥记录选成「私钥口令」看上去毫无异样。 -->
            {#if protocol === "ssh" && passphraseRelevant}
            <label>私钥口令（仅加密私钥需要）
              <select bind:value={passphraseRecord} data-testid="pf-passphrase-select">
                <option value={null}>无（私钥未加密）</option>
                {#each passphraseCandidates as r (r.id)}
                  <option value={r.id}>#{r.id} · {kindLabel(r.kind)} · {r.label}</option>
                {/each}
              </select>
            </label>
            {/if}
            <!-- 「为什么选不了」必须当场说清（用户 2026-09-01 两次报同一件事）。
                 三种情况长得一模一样——下拉里都只有一个「无」——但修法完全不同：
                 ① 读列表失败（最常见是保险库锁着）→ 去解锁，不是去建记录；
                 ② 一条记录都没有 → 去建一条；
                 ③ 有记录但都不是密码类 → 私钥口令只能用密码类（拿私钥解私钥无意义）。
                 此前三者都是静默的空下拉，用户只能得出「这控件坏了」的结论。 -->
            {#if vaultListError}
              <span class="err" role="alert" data-testid="pf-vault-list-error">
                {vaultListError}
                <button type="button" class="linkish" onclick={() => void refreshVaultRecords()}>重试</button>
              </span>
            {:else if protocol === "ssh" && vaultListLoaded && vaultRecords.length === 0}
              <span class="hint" data-testid="pf-vault-empty">
                Vault 里还没有任何凭据记录。用下方「存入 Vault…」建一条：存的是私钥就出现在「使用保险库中的私钥」那一档，
                存的是密码就可以作加密私钥的口令。
              </span>
            {:else if passphraseRelevant && vaultRecords.length > 0 && passphraseCandidates.length === 0}
              <span class="hint" data-testid="pf-no-passphrase-record">
                私钥口令只能用「{KIND_LABEL.password}」类别的记录，而 Vault 里目前没有这样的记录
                （私钥记录不能作口令，所以不列出来）。私钥若是加密的，请用下方「存入 Vault…」
                新建一条密码类记录存放它的口令，再回来选。
              </span>
            {/if}
            {#if selectedPassKind !== undefined && !usableAsPassphrase(selectedPassKind)}
              <span class="err" role="alert" data-testid="pf-pass-kind-warn"
                >私钥口令必须是「{KIND_LABEL.password}」类别的记录，当前选的是「{kindLabel(selectedPassKind)}」：连接时后端会直接拒绝。</span
              >
            {/if}
            {#if protocol === "ssh"}
            <!-- 收起 = 放弃录入，明文当场丢掉（审计2 #29）。旧写法是纯 `!vaultFormOpen` 取反，
                 收起只是把 fieldset 从 DOM 卸掉，`newSecret` 里那段密码/私钥 PEM 一字未动。 -->
            <button type="button" class="import"
                    onclick={() => { if (vaultFormOpen) clearVaultForm(); else vaultFormOpen = true; }}
                    data-testid="pf-vault-toggle">存入 Vault…</button>
            {#if vaultFormOpen}
              <fieldset class="inline-vault">
                <legend>新建凭据（加密保存在保险库）</legend>
                <label>类型
                  <select bind:value={newKind}>
                    <option value="password">密码</option>
                    <option value="private_key">私钥</option>
                  </select>
                </label>
                <label>标签 <input bind:value={newLabel} data-testid="pf-vault-label" /></label>
                {#if newKind === "password"}
                  <label>密码 <input type="password" bind:value={newSecret} data-testid="pf-vault-secret" /></label>
                {:else}
                  <label>私钥文本 <textarea bind:value={newSecret} rows="4" data-testid="pf-vault-secret"></textarea></label>
                {/if}
                <button type="button" class="import" onclick={() => void putSecret()} data-testid="pf-vault-put">保存凭据</button>
                {#if vaultFormError}<span class="err" role="alert">{vaultFormError}</span>{/if}
              </fieldset>
            {/if}
            <!-- 审计2 #24：原文案「允许 Agent 转发」与事实不符——这个开关决定的是认证材料
                 是否取自本地 SSH Agent（协议层就是 publickey），与「转发」（TCP/UNIX 转发）无关。 -->
            <label title="认证时使用本地 SSH Agent 中的密钥（协议层即 publickey，与端口转发无关）"><input type="checkbox" bind:checked={allowAgent} /> 使用本地 SSH Agent 认证</label>
            <label><input type="checkbox" bind:checked={allowKbd} /> 允许交互认证（验证码等）</label>
            <!-- 审计2 #36：语义钉在 tooltip 上——恰一个、不回显（echo=false）的提示才自动应答，
                 多提示/回显提示（新秘密录入）/未配口令一律照旧弹框。 -->
            <label title="仅当服务器只问一个提示、且该提示不回显（echo=false）时，自动以已配置的口令应答；多提示、回显提示（如改密/注册）或未配口令时仍弹框询问"><input type="checkbox" bind:checked={kbdAutoSingle} /> 单提示自动应答</label>
            {/if}
          {:else if tab === "jump"}
            <!-- 审计2 #23：跳板链编辑面。旧实现只会把既有 jump 原样透传，普通用户不能建不能改；
                 导入剥离逐跳凭据与信任断言是有意为之的既有不变式（repo.rs），因此「配了跳板、
                 想要维护」的用户此前没有任何可达入口。 -->
            <div class="jump-rows">
              {#each jumpRows as row, i (row.key)}
                <fieldset class="jump-row" data-testid={`pf-jump-${i}`}>
                  <legend>跳板 {i + 1}</legend>
                  <div class="jump-tools">
                    <button type="button" aria-label="上移" title="上移" disabled={i === 0} onclick={() => moveJump(i, -1)}>↑</button>
                    <button type="button" aria-label="下移" title="下移" disabled={i === jumpRows.length - 1} onclick={() => moveJump(i, 1)}>↓</button>
                    <button type="button" aria-label="删除跳板" title="删除跳板" onclick={() => removeJump(i)}>×</button>
                  </div>
                  <label>主机 <input bind:value={row.host} data-testid={`pf-jump-host-${i}`} placeholder="jump.example.com" /></label>
                  <label>端口 <input type="number" bind:value={row.port} data-testid={`pf-jump-port-${i}`} /></label>
                  <label>用户名 <input bind:value={row.username} data-testid={`pf-jump-user-${i}`} /></label>
                  <label>Vault 凭据
                    <select bind:value={row.vaultRecord} data-testid={`pf-jump-vault-${i}`}>
                      <option value={null}>无</option>
                      {#each vaultRecords as r (r.id)}
                        <option value={r.id} disabled={!usableAsCredential(r.kind)}>#{r.id} · {kindLabel(r.kind)} · {r.label}{usableAsCredential(r.kind) ? "" : "（不能用于 SSH 认证）"}</option>
                      {/each}
                    </select>
                  </label>
                  <!-- 单次查找（@const 需为块直子，故外套一层 {#if}）：列表未加载时 selRec 为
                       undefined，不报警；加载后若指向不可用类别再显式警告——与认证页同口径
                       （置灰挡不住已经存在的选择）。 -->
                  {#if row.vaultRecord != null}
                    {@const selRec = vaultRecords.find((r) => r.id === row.vaultRecord)}
                    {#if selRec && !usableAsCredential(selRec.kind)}
                      <span class="err" role="alert"
                        >这条记录的类别是「{kindLabel(selRec.kind)}」，不能作为 SSH 主凭据：连接这一跳时后端会直接拒绝。</span
                      >
                    {/if}
                  {/if}
                  <label title="认证时使用本地 SSH Agent 中的密钥（协议层即 publickey，与端口转发无关）"><input type="checkbox" bind:checked={row.allowAgent} /> 使用本地 SSH Agent 认证</label>
                  <label>主机密钥策略
                    <select bind:value={row.policy} data-testid={`pf-jump-policy-${i}`}>
                      <option value="">未配置（连接层回落 TOFU）</option>
                      <option value="tofu">TOFU（首次信任）</option>
                      <option value="strict">严格</option>
                      <option value="fingerprint_pinned">指纹钉扎</option>
                    </select>
                  </label>
                  {#if row.policy === "fingerprint_pinned" && row.pins.length === 0}
                    <!-- 与目标机同一陷阱：钉扎 + 空钉 = 这一跳恒硬拒。保存前必须说破。 -->
                    <p class="err" role="alert" data-testid={`pf-jump-pins-warn-${i}`}>
                      策略为「指纹钉扎」但本跳未添加钉扎指纹 —— 这一跳将<strong>无法建立</strong>。
                      请从信任库添加本跳主机的指纹，或改用 TOFU / 严格策略。
                    </p>
                  {/if}
                  <div class="pins">
                    <div class="pins-title">本跳钉扎指纹</div>
                    {#each row.pins as pin (pin.key_blob)}
                      <div class="pin">
                        <code>{pin.fingerprint_sha256}</code>
                        <button aria-label="移除指纹" onclick={() => removeHopPin(i, pin.key_blob)}>×</button>
                      </div>
                    {:else}
                      <p class="hint">无钉扎指纹（TOFU/严格无需）。</p>
                    {/each}
                    <button type="button" class="import" data-testid={`pf-jump-pinbtn-${i}`} onclick={() => void openHopPinPicker(row.key)}>添加钉扎指纹…</button>
                  </div>
                  {#if pickerKey === row.key}
                    <div class="pins" data-testid="pf-hop-pin-picker">
                      <div class="pins-title">信任库中 {row.host || "该跳主机"}:{row.port} 的已知密钥</div>
                      {#if hopPickerError}
                        <p class="err" role="alert">{hopPickerError}</p>
                      {:else}
                        {#each hopPickerKeys as k (k.key_blob)}
                          <div class="pin">
                            <code>{k.key_type} {k.fingerprint_sha256}</code>
                            <button type="button" class="add" onclick={() => addHopPin(i, k)}
                                    disabled={row.pins.some((p) => p.key_blob === k.key_blob)}>钉扎</button>
                          </div>
                        {:else}
                          <p class="hint">信任库中没有这台主机的记录。请先以 TOFU 策略连到这一跳并确认密钥，或导入 known_hosts。</p>
                        {/each}
                      {/if}
                      <button type="button" class="import" onclick={closeHopPinPicker}>收起</button>
                    </div>
                  {/if}
                </fieldset>
              {:else}
                <p class="hint">无跳板：直接连接目标主机。</p>
              {/each}
            </div>
            <button class="import" data-testid="pf-jump-add" disabled={jumpRows.length >= 8}
                    title={jumpRows.length >= 8 ? "跳板数量上限 8（与后端一致）" : ""}
                    onclick={addJump}>＋ 添加跳板</button>
          {:else if tab === "hostkey"}
            <label>策略
              <select bind:value={policy} data-testid="pf-hk-policy" onchange={() => { policyTouched = true; }}>
                <option value="tofu">TOFU（首次信任）</option>
                <option value="strict">严格</option>
                <option value="fingerprint_pinned">指纹钉扎</option>
              </select>
            </label>
            {#if policy === "fingerprint_pinned" && pins.length === 0}
              <!-- 钉扎 + 空钉 = 后端恒硬拒（无密钥可被接受）。这条警告必须在保存前出现：
                   否则用户要到下次连接失败时才知道，而失败信息在 status 通道里未必被注意到。 -->
              <p class="err" role="alert" data-testid="pf-hk-empty-pins">
                策略为「指纹钉扎」但未添加任何钉扎指纹 —— 该连接将<strong>无法建立</strong>
                （空白名单不放行任何密钥）。请添加一条钉扎指纹，或改用 TOFU / 严格策略。
              </p>
            {/if}
            <div class="pins">
              <div class="pins-title">已钉扎指纹</div>
              {#each pins as pin, i (pin.key_blob)}
                <div class="pin">
                  <code>{pin.fingerprint_sha256}</code>
                  <button aria-label="移除指纹" onclick={() => removePin(i)}>×</button>
                </div>
              {:else}
                <p class="hint">无钉扎指纹（TOFU/严格无需）。</p>
              {/each}
            </div>
            <button class="import" onclick={() => void openPinPicker()} data-testid="pf-add-pin">添加钉扎指纹…</button>
            {#if pinPickerOpen}
              <div class="pins" data-testid="pf-pin-picker">
                <div class="pins-title">信任库中 {host || "该主机"}:{port} 的已知密钥</div>
                {#if pinPickerError}
                  <p class="err" role="alert">{pinPickerError}</p>
                {:else}
                  {#each knownKeys as k (k.key_blob)}
                    <div class="pin">
                      <code>{k.key_type} {k.fingerprint_sha256}</code>
                      <button type="button" class="add" onclick={() => addPin(k)}
                              disabled={pins.some((p) => p.key_blob === k.key_blob)}>钉扎</button>
                    </div>
                  {:else}
                    <!-- 空不是错误：新主机本就还没有记录。钉只能来自本机已确认过的密钥，
                         所以正确的下一步是先连一次并在 TOFU 框上确认，而不是手工填指纹。 -->
                    <p class="hint">信任库中没有这台主机的记录。请先以 TOFU 策略连接一次并确认密钥，或导入 known_hosts。</p>
                  {/each}
                {/if}
                <button type="button" class="import" onclick={() => (pinPickerOpen = false)}>收起</button>
              </div>
            {/if}
            <button class="import" onclick={() => fileInput?.click()} data-testid="pf-import-known-hosts">导入 known_hosts…</button>
            <input type="file" accept=".txt,text/plain" bind:this={fileInput}
                   onchange={onImportKnownHosts} style="display:none" />
            {#if importToast}<p class="toast" role="status" data-testid="pf-toast">{importToast}</p>{/if}
          {:else if tab === "terminal"}
            <label>TERM <input bind:value={term} placeholder="xterm-256color" /></label>
            <!-- M7.4：这一栏此前是死配置——一个 disabled 的输入框恒显 UTF-8，而 `term.encoding`
                 在全仓没有任何消费方。国产板子输出 GBK 时用户在这里什么都做不了。
                 清单由后端 `term_encodings` 给：能不能解出来是 Rust 侧 StreamDecoder 说了算。 -->
            <label>编码 <select bind:value={encoding} data-testid="pf-encoding">
              <option value="">UTF-8（默认）</option>
              {#each encodings as [value, human] (value)}
                {#if value !== "utf-8"}<option {value}>{human}</option>{/if}
              {/each}
              <!-- 档案里存着一个清单里没有的标签（手改过 JSON，或清单一时取不到）时，
                   仍要把它列出来并选中。缺这一项的话下拉会静默退回默认，用户按下保存
                   就把自己原来的设置弄丢了，而界面上一句提示都没有。 -->
              {#if encoding && !encodings.some(([v]) => v === encoding)}
                <option value={encoding} data-testid="pf-encoding-extra">{encoding}（档案中已有）</option>
              {/if}
            </select></label>
            <p class="hint">连上之后还可以随时在状态栏点编码段临时换一个，不需要重连。</p>
            <label>滚动行数 <input bind:value={scrollback} data-testid="pf-scrollback" placeholder="10000" /></label>
            <label>字号（8–32，空=跟随全局） <input type="number" min="8" max="32" bind:value={fontSizeInput} placeholder="13" data-testid="pf-font-size" /></label>
            <label>配色方案 <select bind:value={schemeSel} data-testid="pf-scheme">
              <option value="">(跟随全局)</option>
              {#each SCHEMES as s (s.id)}<option value={s.id}>{s.name}</option>{/each}
            </select></label>
          {:else if tab === "sftp"}
            <label>本地目录 <input bind:value={localDir} /></label>
            <label>远程目录 <input bind:value={remoteDir} /></label>
            <label>下载沙箱 <input bind:value={downloadSandbox} /></label>
          {:else if tab === "ai"}
            <p class="hint">AI 功能请在「工具 → 选项（设置）→ AI」中配置。</p>
          {:else if tab === "env"}
            {#each envRows as row, i (i)}
              <div class="envrow">
                <input bind:value={row.key} placeholder="KEY" />
                <input bind:value={row.value} placeholder="value" />
              </div>
            {/each}
            <button onclick={addEnvRow}>＋ 添加环境变量</button>
          {/if}
        </section>
      </div>
      <footer>
        {#if formError}<span class="err" role="alert">{formError}</span>{/if}
        <button disabled={saving} onclick={close}>取消</button>
        <button class="primary" onclick={() => void save()} disabled={saving} data-testid="pf-save">{saving ? "保存中…" : "保存"}</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 50; }
  .dialog { width: 640px; max-width: 92vw; max-height: 86vh; display: flex; flex-direction: column; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); }
  header { display: flex; align-items: center; justify-content: space-between; padding: 10px 14px; border-bottom: 1px solid var(--fs-border); }
  header h2 { margin: 0; font-size: 14px; }
  .x { border: 0; background: transparent; color: var(--fs-fg-secondary); font-size: 16px; cursor: pointer; }
  .body { flex: 1; display: flex; min-height: 0; }
  .tabs { flex: none; width: 130px; display: flex; flex-direction: column; padding: 8px; gap: 2px; border-right: 1px solid var(--fs-border); }
  .tabs button { text-align: left; padding: 6px 8px; border: 0; background: transparent; color: var(--fs-fg-secondary); border-radius: 4px; cursor: pointer; }
  .tabs button[aria-selected="true"] { background: var(--fs-accent-dim); color: var(--fs-accent); }
  .tabs button:disabled { opacity: .5; cursor: not-allowed; }
  .pane { flex: 1; padding: 14px; overflow-y: auto; display: flex; flex-direction: column; gap: 10px; }
  .pane label { display: flex; flex-direction: column; gap: 4px; font-size: 12.5px; color: var(--fs-fg-secondary); }
  /* 行内控件的横排例外：checkbox **和 radio** 都要——2026-08-31 真机报出
   * radio 圆点与文字竖着摞成两行，就是这份例外清单漏了 radio（凭据归属的
   * 两个单选，见 data-testid="pf-cred-own"）。规律同 .sep 那次：竖排 flex 的
   * 容器 + 例外按控件类型枚举 = 每加一种控件都要记得回来扩这份清单，漏一次
   * 就是一次看得见的排版事故。range/color 若进 label 也要加进来。 */
  .pane label:has(input[type="checkbox"], input[type="radio"]) { flex-direction: row; align-items: center; gap: 6px; }
  /* 单选组本身竖排成组（两组二选一，不是横着挤一行） */
  .cred-mode { display: flex; flex-direction: column; gap: 6px; }
  .cred-mode legend { font-size: 12px; color: var(--fs-fg-secondary); padding: 0 4px; }
  .pane input, .pane select, .pane textarea { padding: 5px 8px; background: var(--fs-bg-input); border: 1px solid var(--fs-border); border-radius: 4px; color: var(--fs-fg-primary); }
  /* select 单独补右内距：input/textarea 不该跟着空出 22px（2026-09-01） */
  .pane select { padding-right: 22px; }
  /* 行内「重试」：不抢眼但点得到（错误提示里的当场动作）。 */
  .linkish { background: none; border: none; color: var(--fs-accent); padding: 0 0 0 6px; cursor: pointer; font: inherit; text-decoration: underline; }
  .pins { border: 1px solid var(--fs-border); border-radius: 4px; padding: 8px; }
  .pins-title { font-size: 12px; color: var(--fs-fg-secondary); margin-bottom: 6px; }
  .pin { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .pin code { font-size: 11.5px; }
  .pin button { border: 0; background: transparent; color: var(--fs-fg-secondary); cursor: pointer; font-size: 16px; }
  .pin button.add { font-size: 12px; border: 1px solid var(--fs-border); border-radius: 4px; padding: 2px 8px; color: var(--fs-fg-primary); }
  .pin button.add:disabled { opacity: .45; cursor: default; }
  /* 空钉警告与选取器错误此前落在 `.inline-vault .err` / `footer .err` 之外，等于**没有样式**：
     一条安全警告用正文色渲染，与旁边的说明文字无法区分。危险色是它唯一的可见性来源。 */
  .pane > .err, .pins .err { color: var(--fs-danger); font-size: 12px; margin: 0; }
  .import { align-self: flex-start; padding: 5px 12px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  .toast { font-size: 12px; color: var(--fs-accent); }
  .inline-vault { display: flex; flex-direction: column; gap: 8px; border: 1px solid var(--fs-border); border-radius: 4px; padding: 10px; }
  .inline-vault legend { font-size: 12px; color: var(--fs-fg-secondary); padding: 0 4px; }
  .inline-vault .err { color: var(--fs-danger); font-size: 12px; }
  .hint { font-size: 12px; color: var(--fs-fg-secondary); }
  .envrow { display: flex; gap: 6px; }
  .envrow input { flex: 1; }
  .jump-rows { display: flex; flex-direction: column; gap: 10px; }
  .jump-row { display: flex; flex-direction: column; gap: 8px; border: 1px solid var(--fs-border); border-radius: 4px; padding: 10px; margin: 0; }
  .jump-row legend { font-size: 12px; color: var(--fs-fg-secondary); padding: 0 4px; }
  .jump-tools { display: flex; gap: 4px; justify-content: flex-end; }
  .jump-tools button { border: 0; background: transparent; color: var(--fs-fg-secondary); cursor: pointer; font-size: 13px; padding: 2px 8px; }
  .jump-tools button:disabled { opacity: .4; cursor: default; }
  footer { display: flex; align-items: center; justify-content: flex-end; gap: 8px; padding: 10px 14px; border-top: 1px solid var(--fs-border); }
  footer .err { color: var(--fs-danger); font-size: 12px; margin-right: auto; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button.primary { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: var(--fs-accent); }
</style>
