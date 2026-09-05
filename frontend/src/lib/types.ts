/* ============================================================================
 * Profile 线格式（wire format）——权威定义在 `crates/connmgr/src/model.rs`
 *
 * 本段全部类型是 serde 序列化结果的**逐字对偶**，不是「界面模型」。改动纪律：
 *   · Rust `Option<T>`（无 default）→ TS `T | null`：键**必须在场**，值可为 null。
 *     （serde 对缺失的 `Option` 字段确实会给 None，但把它写成 `?` 会让前端漏发字段
 *      变成合法写法，漂移就此无声；这里一律要求显式发 null。）
 *   · Rust `#[serde(default)]` → TS 可选 `?`：允许前端提交精简载荷，缺键由 Rust 回填默认。
 *   · Rust `enum` + `#[serde(rename_all = "snake_case")]` → TS **字面量联合（裸字符串）**，
 *     绝不是 `{ mode: ... }` 之类的包装对象——旧定义正是栽在这里，serde 直接反序列化失败，
 *     `profile_save` 整个存不进去。
 * 每个字段上的注释标注它对应 Rust 的哪个类型，这是唯一能挡住再次漂移的东西。
 * ========================================================================= */

/** Rust `HostKeyPolicy`（`#[serde(rename_all="snake_case")]` 的裸枚举，**不是对象**）。
 *  注意没有 `accept_new` 这个取值——Rust 侧从来不存在，发过去必然反序列化失败。 */
export type HostKeyPolicy = "tofu" | "strict" | "fingerprint_pinned";

/** Rust `AutoExec`（同为 snake_case 裸枚举）。旧定义把它写成 boolean，`false` 发过去即失败。 */
export type AutoExec = "off" | "read_only" | "with_confirm";

/** Rust `HostKeyPin`：两个字段**都必填、都无 default**。
 *  历史上的 `{ key_type, fingerprint }` 形状与 Rust 毫无交集，任何非空钉列表都会让保存整体失败。 */
export interface HostKeyPin {
  /** Rust `key_blob: String`：主机公钥原文（base64 blob），指纹的原像 */
  key_blob: string;
  /** Rust `fingerprint_sha256: String`：SHA256 指纹，与 `ssh-keygen -lf` 逐字一致 */
  fingerprint_sha256: string;
}

/** Rust `fs_vault::SecretKind::aad_tag()` / `fs_sshengine::secrets::SecretKind::wire_tag()` 的取值集合。
 *  两侧都**不设兜底分支**：认不出的串一律 `None`，故这里的联合必须与 Rust 逐字同集
 *  （由 `enum-contract.test.ts` 的 SecretKind 组钉住）。 */
export type SecretKind = "password" | "private_key" | "api_key";

/** Rust `AuthRef`。注意 Rust 侧**没有** `method` / `key_path` / `prefer_vault`——
 *  用哪种认证方式，取决于所选 vault 记录**自己声明的 `SecretKind`**（`build_credentials` 按
 *  `kind` 分流），不由前端指定，也不再靠猜明文内容。审计2 #20 之前是后者：内容含
 *  `PRIVATE KEY` 才当私钥，于是 `.ppk`（`PuTTY-User-Key-File-3:` 开头、全文无此子串）
 *  会被当作**口令明文发给服务器**。类别不匹配（如把 api_key 选进来）现在是硬错误，
 *  不会静默降级——前端因此要在选取器里就把不可用的记录标出来。 */
export interface AuthRef {
  /** Rust `Option<u64>`：密码或私钥 PEM 的 vault 记录 id。是**数字**，旧定义发的是字符串 */
  vault_record: number | null;
  /** Rust `#[serde(default)] Option<u64>`：加密私钥的口令所在 vault 记录 id；null/缺省 = 私钥未加密 */
  passphrase_vault_record?: number | null;
  /** Rust `#[serde(default)] bool` */
  allow_agent?: boolean;
  /** Rust `#[serde(default)] bool` */
  allow_kbd_interactive?: boolean;
  /** Rust `#[serde(default)] bool`：审计2 #36 已接通——恰一个不回显的 kbd 提示可自动以口令应答 */
  kbd_auto_answer_single?: boolean;
}

/** Rust `JumpHop`。旧定义把 `jump` 写成 `string | null` —— Rust 是 `Vec<JumpHop>`，
 *  显式 `null` 也不行（`#[serde(default)]` 只对**缺键**生效，对 null 不生效），无跳板须发 `[]`。 */
export interface JumpHop {
  host: string;
  /** Rust `u16` */
  port: number;
  username: string;
  /** Rust `#[serde(default)] Protocol`：缺省 ssh（存量数据无此键）。 */
  protocol?: "ssh" | "rdp" | "serial";
  /** Rust `AuthRef`（无 default，必填） */
  auth: AuthRef;
  /** Rust `#[serde(default)] Option<HostKeyPolicy>`：**这一跳自己的**策略；null = 未配置 → 连接层回落 TOFU。
   *  不得沿用目标主机的 policy/pins（指纹钉绑定到具体主机身份，跨主机复用属类型混淆） */
  host_key_policy?: HostKeyPolicy | null;
  /** Rust `#[serde(default)] Vec<HostKeyPin>`：这一跳自己的钉，空 = 本跳无钉 */
  host_key_pins?: HostKeyPin[];
}

/** Rust `TermThemeOverride`（**snake_case 线格式**）。
 *  勿与 `lib/theme/store.ts` 的同名类型混淆：那个是 UI 侧 camelCase（`fontSize`）的合并层模型，
 *  两者之间需要装配层做键名映射（见文件末尾的 App.svelte 接线说明）。 */
export interface TermThemeOverride {
  /** Rust `Option<String>`：配色方案 id */
  scheme: string | null;
  /** Rust `Option<u32>`：字号 px */
  font_size: number | null;
}

/** Rust `TermSettings`。Rust 侧**没有** `cols`/`rows`/`flow_control`/`scheme_id`——
 *  cols/rows 由 PTY 按窗口实测尺寸决定（term_resize），不是档案字段。 */
export interface TermSettings {
  /** Rust `Option<String>`：TERM 环境变量值 */
  term: string | null;
  /** Rust `Option<String>`：v1 仅 UTF-8 直通，控件为只读展示 */
  encoding: string | null;
  /** Rust `Option<u32>` */
  scrollback_lines: number | null;
  /** Rust `#[serde(default)] Option<TermThemeOverride>` */
  theme_override?: TermThemeOverride | null;
}

/** Rust `SftpDefaults`。Rust 侧**没有** `permissions`（远端权限位由传输层按源文件决定）。 */
export interface SftpDefaults {
  /** Rust `Option<String>` */
  local_dir: string | null;
  /** Rust `Option<String>` */
  remote_dir: string | null;
  /** Rust `Option<String>`：下载落盘沙箱根（spec §3.3） */
  download_sandbox: string | null;
}

/** Rust `AiPolicy`。Rust 侧**没有** `allow_command_suggest`。 */
export interface AiPolicy {
  /** Rust `#[serde(default)] AutoExec`：三态枚举，不是 boolean */
  auto_execute?: AutoExec;
  /** Rust `#[serde(default)] bool` */
  mcp_allowed?: boolean;
}

/** Rust `Profile`（`crates/connmgr/src/model.rs`）。 */
/** 串口档案参数（M7.4）。取值与 Rust `fs_connmgr::SerialSettings` 逐字对齐。 */
export interface SerialSettings {
  port: string;
  baud: number;
  /** five | six | seven | eight */
  data_bits: string;
  /** none | odd | even */
  parity: string;
  /** one | two */
  stop_bits: string;
  /** none | software | hardware */
  flow: string;
}

export interface Profile {
  /** Rust `Uuid`：新建时可发全零 UUID，`profile_save` 用**同级 `id` 参数**覆盖它（None → 新建 v4） */
  id: string;
  name: string;
  /** Rust `Option<String>`（无 default，键必须在场） */
  group_path: string | null;
  host: string;
  /** Rust `#[serde(default = "default_port")] u16`（缺省 22）。
   *  保留为必填：`profiles_list` 回来的每条记录都带此键，标成可选只会污染读取侧。 */
  port: number;
  username: string;
  /** Rust `#[serde(default)] Protocol`（缺省 ssh；存量数据无此键） */
  protocol?: "ssh" | "rdp" | "serial";
  /** Rust `#[serde(default)] AuthRef` */
  auth?: AuthRef;
  /** Rust `#[serde(default)] Vec<JumpHop>`：无跳板发 `[]`，**不是 null** */
  jump?: JumpHop[];
  /** Rust `#[serde(default)] HostKeyPolicy`：裸字符串 */
  host_key_policy?: HostKeyPolicy;
  /** Rust `#[serde(default)] Vec<HostKeyPin>` */
  host_key_pins?: HostKeyPin[];
  /** Rust `#[serde(default)] BTreeMap<String, String>` */
  env?: Record<string, string>;
  /** Rust `#[serde(default)] TermSettings` */
  term?: TermSettings;
  /** Rust `#[serde(default)] SftpDefaults` */
  sftp?: SftpDefaults;
  /** Rust `#[serde(default)] AiPolicy` */
  ai_policy?: AiPolicy;
  /** Rust `#[serde(default)] SerialSettings`（M7.4；仅 protocol=serial 时有意义） */
  serial?: SerialSettings;
}

/** `profiles_list` 的返回体：**对象**而非数组（Rust `ProfileListResult`，`bad_rows` 经 Tauri
 *  camelCase 化为 `badRows`）。坏行必须在列表末尾可见呈现，不得只进 console。 */
export interface ProfileListResult {
  profiles: Profile[];
  badRows: { id: string; error: string }[];
}

export interface TransferEvent {
  id: number;
  bytes_done: number;
  bytes_total: number;
  state: "Running" | "Done" | "Cancelled" | { Failed: string } | { Retrying: { attempt: number } }; // R109：`Cancelled` 与 Rust `TransferState::Cancelled` 对偶（transfer_cancel 后事件泵即发此态，Task 21 TransferQueueDrawer 已按此渲染）
}

export interface AuthPromptPayload {
  session_id: string;
  // 本回合提问的 nonce（审计 P1-24）。后端待决表键为 `(session_id, prompt_id)`，应答必须原样回传；
  // 声明为可选是与运行时的 `?? null` 兜底对齐——事件跨 IPC 边界进来，TS 类型不构成任何强制。
  promptId?: string;
  // `user@host:port`，由后端按**本地连接配置**拼出（app/src/events.rs `target_label`）。
  // 与下面的 name/instruction 分属两个信任级别：那两项是服务端自报的，不能当身份用。
  target?: string;
  name?: string;         // russh InfoRequest.name 原样透传
  instruction?: string;  // russh InfoRequest.instructions 原样透传（总设计 §2.1 v3 / UI 规格 §2.5：空值折叠对应区域）
  prompts: { text: string; echo: boolean }[];
  // 连接时输口令（Task 47）：authKind === "password" 时这是客户端主动发起的口令询问（区别于服务端 kbd-interactive），
  // 前端据此显示「记住（存入 Vault）」勾选；profileId 供勾选后回写 auth.vault_record。
  // 命名避开裸 `kind`：那是 hostkey:prompt 的字段（tofu/changed），两个事件不该共用一个契约名。
  authKind?: "password";
  profileId?: string | null;
}

/** 系统监控快照（M4a）。逐字对应 Rust `fs_sshengine::monitor::MonitorSnapshot`（serde 默认 snake_case；
 * cpu_times 带 #[serde(skip)] 不出线，前端只见算好的百分比）。 */
export interface MonitorSnapshot {
  hostname: string;
  uptime: string;
  load_1?: number | null;
  load_5?: number | null;
  load_15?: number | null;
  mem_used_mb?: number | null;
  mem_total_mb?: number | null;
  disk_used: string;
  disk_total: string;
  /** CPU%：后端跨轮差量（S310）。首轮无前值 / 非 Linux 降级为 null——渲染「—」，不是 0。 */
  cpu_percent?: number | null;
  /** CPU 核数（M4b）。异常判定要用它把负载换算成「每核」——`load_1 = 5`
   *  在单核上是五倍过载、在 64 核上几乎空闲。null = 采不到，那时**不判**负载。 */
  cpu_cores?: number | null;
}

export interface HostKeyPromptPayload {
  session_id: string;
  promptId?: string;        // 同上：本回合提问的 nonce，`hostkey_decide` 据此定位（审计 P1-24）
  host: string;
  port: number;
  key_type: string;         // 密钥类型（ssh-ed25519 等），UI §2.5 对话框呈现项
  fingerprint: string;        // 当前主机密钥指纹（SHA256:…，与 ssh-keygen -lf 逐字一致）
  kind: "tofu" | "changed";
  old_fingerprint?: string | null;  // kind=changed 时为被替换的旧键指纹，供 HostKeyDialog 新旧对比（R15；tofu 时为 null）
}

/// `hostkey_import` 的返回值，逐字对应 Rust 侧 `fs_sshengine::hostkey::ImportSummary`
/// （serde 默认按字段名序列化，故这里用 snake_case）。
///
/// 审计2 #22：旧结构只有 `imported / skipped / upgraded / conflicted`，那个 `skipped`
/// 同时装着「本来就在库里」和「整行安全信息被丢掉」两类性质完全相反的结局，用户无从分辨。
/// 现在每一格都有单一含义，前端逐项措辞——**没有导入的那几类必须明说没有生效**。
export interface ImportSummary {
  imported: number;
  upgraded: number;
  conflicted: number;
  duplicate: number;
  revoked: number;
  cert_authority: number;
  hashed: number;
  pattern: number;
  malformed: number;
}

export interface VaultEntry {
  id: string;
  host: string;
  port: number;
  username: string;
  auth_type: "password" | "key";
  key_path?: string | null;
}

export interface ProfileFormData {
  name: string;
  group_path: string;
  host: string;
  port: number;
  username: string;
}

export interface VaultFormData {
  host: string;
  port: number;
  username: string;
  auth_type: "password" | "key";
  password?: string;
  key_path?: string;
}

/**
 * `session_open` 失败时的**结构化** rejection（Rust `app/src/connect_failure.rs`，2026-09-01）。
 *
 * 此前失败只回一句字符串。要让失败面板挂出「选私钥 / 用 Agent 重试 / 重试」这些能点的
 * 东西，前端必须结构化地知道服务器通告了什么、我们试过什么、失败属于哪一类。
 * `summary` 是 Rust `Error` 的 Display 原文，一字不改——errorText / toast / 日志都用它。
 * `remaining` 已在后端过滤掉本地合成的 `agent`（服务器从不通告那个名字）。
 * `category` 取值与 Rust `category_of` 由 enum-contract.test.ts 逐字对齐。
 */
export interface ConnectFailure {
  summary: string;
  category: "auth" | "secret_locked" | "host_key" | "timeout" | "connect" | "other";
  tried: string[];
  remaining: string[];
  notes: string[];
  host: string;
  port: number;
  username: string;
  /** 主机密钥 SHA256 指纹；未连过为空串 */
  fingerprint: string;
  has_jump: boolean;
}
