// settings_cmd.rs
use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

/// 读取单项设置（值为 JSON 字符串；不存在返回 None）。SettingsRepo 由 connmgr 提供（G5）。
#[tauri::command]
pub async fn settings_get(
    key: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<String>, String> {
    fs_connmgr::SettingsRepo::new(state.db.pool())
        .get(&key)
        .await
        .map_err(|e| e.to_string())
}

/// 写入单项设置（upsert；值为 JSON 字符串，结构化配置序列化为 JSON）。
///
/// 审计2 #40：写入前先过**运行期 schema**（键白名单 + 每键值规则，见 [`validate_setting`]）。
/// 旧实现是裸透传：前端泛型只做 TypeScript 断言，不验证持久化 JSON 的实际结构，
/// 损坏或越界值进库后各消费者产生不一致的回退行为。现在键不在表上、值不合规则
/// 一律拒绝写入——**不改库**，文案直出给调用方。
#[tauri::command]
pub async fn settings_set(
    key: String,
    value: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    validate_setting(&key, &value)?;
    fs_connmgr::SettingsRepo::new(state.db.pool())
        .set(&key, &value)
        .await
        .map_err(|e| e.to_string())
}

// ───────────── 审计2 #40：设置键白名单与运行期 schema ─────────────

/// 设置键名长度上限（防库膨胀的最小边界；真实键最长的 `security.clipboardClear` 也远够不到）。
const SETTING_KEY_CHARS_MAX: usize = 64;
/// 设置值（JSON 文本）字节上限。8 KiB 是通用爆炸半径闸；单键的精确上限
/// （如 [`SANDBOX_ROOT_CHARS_MAX`] 的 4096 字符）**小于**它，语义边界归各键规则，
/// 本闸只兜「谁也不该这么大的值」。
const SETTING_VALUE_BYTES_MAX: usize = 8192;

/// `sftp.sandboxRoot` 的字符上限，与 Profile 校验的 [`fs_connmgr::model::limits::PATH_MAX`]
/// 同值同源——路径长度的产品边界只有一条，不该有两份数字。
const SANDBOX_ROOT_CHARS_MAX: usize = 4096;

/// `security.clipboardClear.seconds` 的合法区间 [1, 3600]，
/// 与 `vault_cmd::CLIPBOARD_CLEAR_MAX_SECONDS` 的钳位上界逐字一致（1 秒下界见其注释：
/// 0 秒等于「复制即清」，不可能是任何人的本意）。写侧按**拒绝**而不是钳位：
/// 越界值不该落库；读侧钳位仍保留，为库里已存在的旧行兜底。
const CLIPBOARD_SECONDS_MAX: u64 = 3600;

/// `term.scheme` 的合法取值 = 前端 `lib/term-schemes/*.json` 的全部 id。
/// 与前端 12 套内置方案的同步由跨语言守卫测试钉住（读 .json 提取 id 比对本表）。
const TERM_SCHEME_IDS: &[&str] = &[
    "dracula",
    "futureshell-dark",
    "gruvbox-dark",
    "monokai-pro",
    "nord",
    "one-dark",
    "solarized-dark",
    "solarized-light",
    "tomorrow-night",
    "ubuntu",
    "windows-terminal-light",
    "xshell-standard",
];

/// `ui.theme` 的合法取值 = 前端 `lib/theme/themes.ts` 的全部主题 id（另加 "auto"）。
/// 与前端主题表的同步由跨语言守卫测试钉住（读 themes.ts 提取 id 比对本表）。
const THEME_IDS: &[&str] = &["obsidian", "daylight", "slate", "hc"];

/// `ui.language` 的合法取值 = 前端 `lib/i18n/index.ts` 的 `SUPPORTED_LOCALES` 全部 id。
///
/// 与前端的同步由跨语言守卫测试钉住（读那个文件提取 id 比对本表）。两边走散的后果很具体：
/// 用户在下拉框里选得到的语言被后端拒收，而错误文案讲的是「取值非法」——
/// 与他刚做的动作（从一个我们自己给的列表里选了一项）完全对不上。
const LANGUAGE_IDS: &[&str] = &["zh-CN", "en"];

/// `vault.autoLockMinutes` 的合法取值。界面 `<select>` 恰好给出这三项（总设计 §3.2），
/// 别的数字进库 = 库里躺着一个界面上永远选不出来的承诺（P1-16 同族缺陷）。
const AUTO_LOCK_MINUTES: &[u64] = &[0, 5, 30];

/// 白名单全集。**维护铁律**：新增设置键必须同时——
/// ① 在此表加一项；② 在 [`validate_setting`] 的 match 里加一条值规则；
/// ③ 前端经 `settingSet` 写入。跨语言守卫测试会自动发现「前端写了、这里没登记」的漂移；
/// 故意不上表的键（如注释掉的 `term.ghost`）会**响亮地失败**——死配置不该能静默入库。
const ALLOWED_SETTING_KEYS: &[&str] = &[
    // AI provider 配置（M2）：`[{id,kind,base_url,model,key_ref?,allow_screen_context}]`。
    // **不含 API key**——key 存 Vault，这里只留一个记录 id。settings 表是明文 JSON，
    // 而「导出设置」「贴日志求助」是用户每天都在做的两件事。
    // 校验分支里有一条黑名单挡住 api_key/token/secret 这些字段名，见下方。
    "ai.providers",
    // AI 执行档位（总设计 §4.3 总开关）：disabled / read_only / with_confirm。
    // 串与 `fs_policy::AiMode::as_str()` 逐字一致（**下划线版**，不是连字符版）。
    "ai.mode",
    "hostkey.defaultPolicy",
    // 键盘配置文件（M4a）：`{"Ctrl+N": "session.new", "Ctrl+F": null}` 的对象。
    // 值为 null = 显式解绑默认键。键位串的**规范形**由前端 lib/keymap.ts 负责
    // （修饰键固定序 Ctrl→Alt→Shift），本闸只挡形状与包络——规范化是前端的
    // 职责边界（它握有 KeyboardEvent），后端重写一份规范化即是两份实现迟早分叉。
    "keyboard.bindings",
    "keyboard.mode",
    // MCP 对外服务（M3 出口 5，总设计 §4.5）。默认关；三键在 MCP 服务启动当刻读。
    //   mcp.enabled  布尔。
    //   mcp.caller   调用方标签（进审计 `mcp:{client}`，展示用、非安全边界）。
    //   mcp.tools    授权工具名单（字符串数组，元素为线上工具名）。
    "mcp.enabled",
    "mcp.caller",
    "mcp.tools",
    // 外部 MCP server 挂载表（M3 出口 6）：`[{server_id,command,args}]`。
    "mcp.mounts",
    // 快速命令集/片段库（M4a）：`[{id,name,command,category?}]` 的 JSON 数组。
    // 8 KiB 总闸（SETTING_VALUE_BYTES_MAX）同时是条目数的天然上界——单条几十字节，
    // 8 KiB 装得下一百多条，远超「快速命令」的合理用量；FinalShell 式重型片段库
    // 若将来需要更大容量，应升级为独立表而非放宽这条闸。
    // 危险动作的「以后不再显示」豁免（M7.2）：`DangerousAction` 稳定串数组。
    // 白名单里只放这一个键而不是按类别各放一个：类别集合会随 M7.1/M7.3 增长，
    // 每加一档就要改一次后端白名单的话，迟早出现「前端有这一档、后端不认」的静默失败。
    "confirm.suppressed",
    "quick.commands",
    "security.clipboardClear",
    // 会话纯文本日志（M4a）：`{enabled,dir,template,append}`。默认关——转录把命令
    // 与输出**明文**落盘，必须是用户显式选择（读侧 SessionLogConfig::default 同口径）。
    "session.log",
    // 手动新建的会话树文件夹（M4a）：字符串数组。空文件夹靠它存在——分组本身
    // 由 group_path 派生，纯派生下空目录没有存身之处。
    "sidebar.folders",
    "sidebar.hostProbeSeconds",
    "sftp.sandboxRoot",
    // 高亮关键字集（M4a）：`[{id,name,pattern,kind,color,alert,enabled}]`。正则的**合法性**
    // 由前端编译时判定（编译失败整条禁用并标错，见 lib/highlights.ts）——后端不重写
    // 一份正则引擎，只挡形状与包络。
    "term.highlights",
    // 终端内传输（M4a rz/sz）：`{enabled,autoReceive}`。默认全开——用户在终端里
    // 亲手敲 `sz`/`rz` 就是显式意图，不响应才是意外（与 session.log 的「默认关」
    // 相反，两者默认风险不同，见 state::ZmodemConfig）。
    "term.zmodem",
    // RDP 帧投递路径（4d）："raw"（默认，tauri Channel 裸字节）| "event"
    //（JSON+base64 事件，对照与排查用）。两档都在同一构建里，真机 A/B 对比
    //（FS_RDP_FRAMESTATS=1 的判据）不用换版本。
    "rdp.frameTransport",
    // 自定义/导入的终端配色（M4b）：`[{name,foreground,background,ansi[16],cursor?}]`。
    // 与内置的 12 套（前端 `lib/term-schemes/*.json`）分开存——内置的随程序版本走，
    // 这些属于用户数据。混在一起的话，升级时要么覆盖用户导入的，要么不敢更新内置的。
    "term.customSchemes",
    "term.bell",
    "term.fontSize",
    "term.opacity",
    "term.scheme",
    "transfer.verifyAfterTransfer",
    "ui.copyOnSelect",
    "ui.ctrlVPaste",
    "ui.density",
    "ui.multilinePasteConfirm",
    // 界面语言（M4b i18n）："zh-CN" / "en"。切换即时生效，不需要重启。
    "ui.language",
    "ui.restoreUnclosed",
    "ui.rightClick",
    // 手动更新检查地址（M4b）。**空串 = 永不联网**，这是 README「出站面」那一条承诺的
    // 存放处，因此它既要允许为空，又要在非空时限死 https——见下方校验分支。
    "update.manifestUrl",
    // 侧栏停靠侧（M4b）："left" / "right" / "float"。窄屏覆盖层是**自动**的，
    // 不写回这个键——写回会让「拉窄一次就永久变成浮动」，而用户没做过那个选择。
    "ui.sidebarDock",
    // 工具栏自定义（M4b）：{hidden:[id], order:[id]}。顺序存 id 列表而不是给每项存序号——
    // 序号方案在增删按钮时会散架（新按钮没有序号、删掉的留下空洞），而按钮集会随版本变。
    "ui.toolbarLayout",
    "ui.sidebarWidth",
    "ui.theme",
    "vault.autoLockMinutes",
];

/// 运行期 schema：键白名单 + 每键值规则。返回人可读的第一条违规描述（中文，直出 UI）。
///
/// 值一律是**JSON 文本**（前端 `settingSet` 经 `JSON.stringify` 序列化），所以规则的
/// 判据是「序列化后的形态」——`term.fontSize` 的值是 `"13"`（数字），`vault.autoLockMinutes`
/// 的值是 `"\"5\""`（字符串）或 `"5"`（数字，兼容历史两种写法，读侧也两写都认）。
///
/// 只校验形状与能力包络，不校验语义之间的一致性（如 scheme 与主题是否搭配）——
/// 那是组合问题，不在单键 schema 的职责内。
///
/// `pub(crate)`：`import_cmd` 落库自定义配色时走的是 `SettingsRepo::set` 而不是本文件的
/// `settings_set` 命令，若不共用这一个校验，同一个键就有两条口径不同的入库路径——
/// 而绕过校验的那条恰好是**处理外来文件**的那条。
pub(crate) fn validate_setting(key: &str, value_json: &str) -> Result<(), String> {
    if key.chars().count() > SETTING_KEY_CHARS_MAX {
        return Err(format!("设置键长度超过上限 {SETTING_KEY_CHARS_MAX}"));
    }
    if value_json.len() > SETTING_VALUE_BYTES_MAX {
        return Err(format!("设置值超过 {SETTING_VALUE_BYTES_MAX} 字节上限"));
    }
    // 白名单先挡键：下面的 match 只负责「值长什么样」，两层的职责不混。
    // 键不在表上、值规则没写，都是**响亮失败**——故意不上表的死配置不该能静默入库。
    if !ALLOWED_SETTING_KEYS.contains(&key) {
        return Err(format!("未知设置键：{key}"));
    }
    let v: serde_json::Value =
        serde_json::from_str(value_json).map_err(|e| format!("设置值不是合法 JSON：{e}"))?;

    /// JSON 字符串且 ∈ 枚举集。
    fn check_str(v: &serde_json::Value, allowed: &[&str], what: &str) -> Result<(), String> {
        match v.as_str() {
            Some(s) if allowed.contains(&s) => Ok(()),
            Some(s) => Err(format!("{what} 取值非法：{s}")),
            None => Err(format!("{what} 必须是字符串")),
        }
    }
    /// JSON u64 且 ∈ 闭区间。
    fn check_range(v: &serde_json::Value, lo: u64, hi: u64, what: &str) -> Result<(), String> {
        match v.as_u64() {
            Some(n) if (lo..=hi).contains(&n) => Ok(()),
            Some(n) => Err(format!("{what} 取值越界：{n}（允许 {lo}..={hi}）")),
            None => Err(format!("{what} 必须是非负整数")),
        }
    }
    /// JSON 布尔。
    fn check_bool(v: &serde_json::Value, what: &str) -> Result<(), String> {
        match v.as_bool() {
            Some(_) => Ok(()),
            None => Err(format!("{what} 必须是布尔值")),
        }
    }
    /// JSON 字符串且字符数 ≤ max。
    fn check_str_len(v: &serde_json::Value, max: usize, what: &str) -> Result<(), String> {
        match v.as_str() {
            Some(s) if s.chars().count() <= max => Ok(()),
            Some(_) => Err(format!("{what} 长度超过上限 {max}")),
            None => Err(format!("{what} 必须是字符串")),
        }
    }

    match key {
        // 档位串必须与 fs_policy 的稳定串一致。写成连字符版（read-only）的话
        // from_str_exact 认不出、回落 Disabled——症状是「选了只读，AI 却整个不能用」。
        "ai.mode" => check_str(&v, &["disabled", "read_only", "with_confirm"], "ai.mode"),
        // provider 列表：形状 + 包络。**不校验 base_url 能不能连通**（那是网络问题，
        // 不是形状问题），但校验它是 http(s)——一个 file:// 的 base_url 会让 reqwest
        // 报一句与配置完全无关的错，而用户会去查网络。
        "ai.providers" => {
            let arr = v.as_array().ok_or("ai.providers 必须是数组")?;
            if arr.len() > 16 {
                return Err(format!("模型源 {} 个超过上限 16", arr.len()));
            }
            for (i, item) in arr.iter().enumerate() {
                let o = item
                    .as_object()
                    .ok_or_else(|| format!("ai.providers[{i}] 必须是对象"))?;
                for field in ["id", "kind", "base_url", "model"] {
                    let s = o
                        .get(field)
                        .and_then(|x| x.as_str())
                        .ok_or_else(|| format!("ai.providers[{i}].{field} 必须是字符串"))?;
                    if s.chars().count() > 512 {
                        return Err(format!("ai.providers[{i}].{field} 超过 512 字符"));
                    }
                }
                let kind = o["kind"].as_str().unwrap_or_default();
                if !["anthropic", "openai", "ollama"].contains(&kind) {
                    return Err(format!("ai.providers[{i}].kind 取值非法：{kind}"));
                }
                let url = o["base_url"].as_str().unwrap_or_default();
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return Err(format!(
                        "ai.providers[{i}].base_url 要以 http:// 或 https:// 开头"
                    ));
                }
                // **这里挡住把 key 写进 settings 的写法。**
                //
                // 唯一允许的密钥相关字段是 `key_ref`（一个 Vault 记录 id）。哪天有人
                // 图省事加一个 `api_key` 字段，写入这一步就会被拒——而那比事后在
                // 用户贴出来的日志里发现 key 好得多。这道闸挡的是**将来的自己**。
                for banned in [
                    "api_key",
                    "apiKey",
                    "key",
                    "secret",
                    "token",
                    "password",
                    "passphrase",
                ] {
                    if o.contains_key(banned) {
                        return Err(format!(
                            "ai.providers[{i}] 不得含 {banned} 字段——API key 存 Vault，这里只放 key_ref"
                        ));
                    }
                }
                match o.get("key_ref") {
                    None | Some(serde_json::Value::Null) => {}
                    Some(x) => {
                        x.as_u64()
                            .ok_or_else(|| format!("ai.providers[{i}].key_ref 必须是非负整数"))?;
                    }
                }
                match o.get("allow_screen_context") {
                    None => {}
                    Some(x) => {
                        x.as_bool().ok_or_else(|| {
                            format!("ai.providers[{i}].allow_screen_context 必须是布尔值")
                        })?;
                    }
                }
            }
            Ok(())
        }
        "hostkey.defaultPolicy" => check_str(&v, &["tofu", "strict"], "hostkey.defaultPolicy"),
        "keyboard.mode" => check_str(&v, &["remote", "local"], "keyboard.mode"),
        // 键盘配置文件（M4a）：对象，键 = 键位串（1..=48 字符），值 = 动作 id 字符串
        // （1..=64）或 null（显式解绑）。条目数上限 200——一份键位表远用不到，
        // 上界只为挡「把整个库塞进一个键」。
        "keyboard.bindings" => {
            let obj = v.as_object().ok_or("keyboard.bindings 必须是对象")?;
            if obj.len() > 200 {
                return Err(format!(
                    "keyboard.bindings 条目数 {} 超过上限 200",
                    obj.len()
                ));
            }
            for (combo, action) in obj {
                if combo.is_empty() || combo.chars().count() > 48 {
                    return Err(format!(
                        "keyboard.bindings 键位串「{combo}」长度须在 1..=48"
                    ));
                }
                match action {
                    serde_json::Value::Null => {}
                    serde_json::Value::String(s) => {
                        if s.is_empty() || s.chars().count() > 64 {
                            return Err(format!(
                                "keyboard.bindings[{combo}] 动作 id 长度须在 1..=64"
                            ));
                        }
                    }
                    _ => {
                        return Err(format!(
                            "keyboard.bindings[{combo}] 必须是字符串或 null（null = 解绑）"
                        ))
                    }
                }
            }
            Ok(())
        }
        // MCP 对外服务（M3 出口 5）：开关是布尔；调用方标签是短串；工具名单是
        // 字符串数组。**不在这里校验工具名是否真实存在**——`Authorization::new`
        // 会把不存在的名字归入 unknown 而不生效，此处只挡坏形状（与 quick.commands
        // 「后端挡形状、语义归消费侧」同一分工）。
        "mcp.enabled" => check_bool(&v, "mcp.enabled"),
        "mcp.caller" => check_str_len(&v, 64, "mcp.caller"),
        "mcp.tools" => {
            let arr = v.as_array().ok_or("mcp.tools 必须是数组")?;
            if arr.len() > 64 {
                return Err(format!("mcp.tools 条目数 {} 超过上限 64", arr.len()));
            }
            for (i, item) in arr.iter().enumerate() {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("mcp.tools[{i}] 必须是字符串"))?;
                if s.is_empty() || s.chars().count() > 64 {
                    return Err(format!("mcp.tools[{i}] 工具名长度须在 1..=64"));
                }
            }
            Ok(())
        }
        // 外部 MCP 挂载表：`[{server_id, command, args?}]`。server_id/command 必填且为短串；
        // args 是字符串数组。不校验 command 是否存在（那是运行期的事，启动失败会有明确报错）。
        "mcp.mounts" => {
            let arr = v.as_array().ok_or("mcp.mounts 必须是数组")?;
            if arr.len() > 32 {
                return Err(format!("mcp.mounts 条目数 {} 超过上限 32", arr.len()));
            }
            for (i, item) in arr.iter().enumerate() {
                let o = item
                    .as_object()
                    .ok_or_else(|| format!("mcp.mounts[{i}] 必须是对象"))?;
                for field in ["server_id", "command"] {
                    let s = o
                        .get(field)
                        .and_then(|x| x.as_str())
                        .ok_or_else(|| format!("mcp.mounts[{i}].{field} 必须是字符串"))?;
                    if s.is_empty() || s.chars().count() > 256 {
                        return Err(format!("mcp.mounts[{i}].{field} 长度须在 1..=256"));
                    }
                }
                if let Some(args) = o.get("args") {
                    let args = args
                        .as_array()
                        .ok_or_else(|| format!("mcp.mounts[{i}].args 必须是数组"))?;
                    if args.len() > 64 {
                        return Err(format!("mcp.mounts[{i}].args 超过 64 个"));
                    }
                    for (j, a) in args.iter().enumerate() {
                        let s = a
                            .as_str()
                            .ok_or_else(|| format!("mcp.mounts[{i}].args[{j}] 必须是字符串"))?;
                        if s.chars().count() > 512 {
                            return Err(format!("mcp.mounts[{i}].args[{j}] 超过 512 字符"));
                        }
                    }
                }
            }
            Ok(())
        }
        // 快速命令集/片段库（M4a）：数组形状 + 字段包络 + id 唯一。
        // 字段规则与前端 lib/quick-commands.ts 的序列化是同一契约的两侧；
        // 这里挡「进库的坏形状」，前端挡「构造时的坏形状」，两侧规则漂移由
        // canonical 值测试钉住（本文件 tests）。
        "quick.commands" => {
            let arr = v.as_array().ok_or("quick.commands 必须是数组")?;
            let mut seen_ids = std::collections::HashSet::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let what = |f: &str| format!("quick.commands[{i}].{f}");
                let obj = item.as_object().ok_or_else(|| what("必须是对象"))?;
                let id = obj
                    .get("id")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| what("id 必须是字符串"))?;
                if id.is_empty() || id.chars().count() > 64 {
                    return Err(format!("{}：长度须在 1..=64", what("id")));
                }
                if !seen_ids.insert(id.to_string()) {
                    return Err(format!("{}：id 重复（{id}）", what("id")));
                }
                let name = obj
                    .get("name")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| what("name 必须是字符串"))?;
                if name.is_empty() || name.chars().count() > 64 {
                    return Err(format!("{}：长度须在 1..=64", what("name")));
                }
                let command = obj
                    .get("command")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| what("command 必须是字符串"))?;
                if command.is_empty() || command.chars().count() > 2048 {
                    return Err(format!("{}：长度须在 1..=2048", what("command")));
                }
                if let Some(cat) = obj.get("category") {
                    match cat.as_str() {
                        Some(c) if c.chars().count() <= 64 => {}
                        Some(_) => return Err(format!("{}：长度超过上限 64", what("category"))),
                        None => return Err(format!("{} 必须是字符串", what("category"))),
                    }
                }
            }
            Ok(())
        }
        // M7.2：只接受**已知类别**的字符串数组。认不出的串一律拒——写进去的话前端
        // `parseSuppressed` 会把它过滤掉，于是库里躺着一条永远不生效的豁免，
        // 而用户在设置页看不到它、也撤销不了。宁可在写入这一步就说不行。
        "confirm.suppressed" => {
            const KINDS: &[&str] = &[
                "process.kill",
                "service.stop",
                "service.restart",
                "script.run",
                "fs.delete",
                "fs.share",
            ];
            let arr = v.as_array().ok_or("confirm.suppressed 必须是数组")?;
            if arr.len() > KINDS.len() {
                return Err("confirm.suppressed 条目数超过已知动作类别数".to_string());
            }
            for item in arr {
                let k = item
                    .as_str()
                    .ok_or("confirm.suppressed 的每一项必须是字符串")?;
                if !KINDS.contains(&k) {
                    return Err(format!("confirm.suppressed 含未知动作类别 {k}"));
                }
            }
            Ok(())
        }
        "security.clipboardClear" => {
            let obj = v.as_object().ok_or("security.clipboardClear 必须是对象")?;
            match obj.get("enabled").and_then(|e| e.as_bool()) {
                Some(_) => {}
                None => return Err("security.clipboardClear.enabled 必须是布尔值".to_string()),
            }
            check_range(
                obj.get("seconds").unwrap_or(&serde_json::Value::Null),
                1,
                CLIPBOARD_SECONDS_MAX,
                "security.clipboardClear.seconds",
            )
        }
        "sftp.sandboxRoot" => check_str_len(&v, SANDBOX_ROOT_CHARS_MAX, "sftp.sandboxRoot"),
        "term.bell" => check_str(&v, &["badge"], "term.bell"),
        "term.highlights" => {
            let arr = v.as_array().ok_or("term.highlights 必须是数组")?;
            if arr.len() > 64 {
                return Err(format!("term.highlights 条目数 {} 超过上限 64", arr.len()));
            }
            for (i, item) in arr.iter().enumerate() {
                let what = |f: &str| format!("term.highlights[{i}].{f}");
                let obj = item.as_object().ok_or_else(|| what("必须是对象"))?;
                for key in ["id", "pattern"] {
                    let s = obj
                        .get(key)
                        .and_then(|x| x.as_str())
                        .ok_or_else(|| format!("{} 必须是字符串", what(key)))?;
                    if s.is_empty() || s.chars().count() > 200 {
                        return Err(format!("{}：长度须在 1..=200", what(key)));
                    }
                }
                match obj.get("kind").and_then(|x| x.as_str()) {
                    Some("literal") | Some("regex") => {}
                    _ => return Err(format!("{} 只能是 literal 或 regex", what("kind"))),
                }
            }
            Ok(())
        }
        // 自定义配色（M4b 导入 + 配色编辑器共用）。这些值有一部分来自**外来文件**，
        // 所以形状校验比别的键更要紧：颜色串会被直接塞进 CSS 变量与终端主题对象。
        // 只认 `#rrggbb`——不认 `rgb()`、不认颜色名、不认 8 位带 alpha，因为一个能穿过
        // 校验的畸形串最终会变成一段注入 style 的文本。
        "term.customSchemes" => {
            // **只认小写**。这不是吹毛求疵：前端 `term-schemes/index.ts` 的 `HEX6` 是
            // `/^#[0-9a-f]{6}$/`（12 套内置方案一直是这个口径），大写串会被它静默过滤掉。
            // 后端宽容、前端严格的组合最坏——导入报告说「配色 1 套」，设置页里却什么都没多出来，
            // 而中间没有任何一处报错。存储层只该有一种表示形式；写入方（导入引擎已 `to_ascii_lowercase`，
            // 将来的配色编辑器同理）负责规范化。
            fn hex6(v: Option<&serde_json::Value>, what: &str) -> Result<(), String> {
                let s = v
                    .and_then(|x| x.as_str())
                    .ok_or(format!("{what} 必须是字符串"))?;
                let ok = s.len() == 7
                    && s.starts_with('#')
                    && s[1..]
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
                if ok {
                    Ok(())
                } else {
                    Err(format!(
                        "{what} 必须是 #rrggbb 形式的小写十六进制，实得 {s}"
                    ))
                }
            }
            let arr = v.as_array().ok_or("term.customSchemes 必须是数组")?;
            if arr.len() > 24 {
                return Err(format!("自定义配色 {} 套超过上限 24", arr.len()));
            }
            for (i, item) in arr.iter().enumerate() {
                let o = item
                    .as_object()
                    .ok_or_else(|| format!("term.customSchemes[{i}] 必须是对象"))?;
                let name = o
                    .get("name")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| format!("term.customSchemes[{i}].name 必须是字符串"))?;
                if name.is_empty() || name.chars().count() > 64 {
                    return Err(format!("term.customSchemes[{i}].name 长度须在 1..=64"));
                }
                hex6(
                    o.get("foreground"),
                    &format!("term.customSchemes[{i}].foreground"),
                )?;
                hex6(
                    o.get("background"),
                    &format!("term.customSchemes[{i}].background"),
                )?;
                let ansi = o
                    .get("ansi")
                    .and_then(|x| x.as_array())
                    .ok_or_else(|| format!("term.customSchemes[{i}].ansi 必须是数组"))?;
                // 恰好 16 个，不是「至少」——少一个会让某个色号取到 undefined，
                // 多一个说明解析出了别的东西。
                if ansi.len() != 16 {
                    return Err(format!(
                        "term.customSchemes[{i}].ansi 必须恰好 16 项，实得 {}",
                        ansi.len()
                    ));
                }
                for (j, c) in ansi.iter().enumerate() {
                    hex6(Some(c), &format!("term.customSchemes[{i}].ansi[{j}]"))?;
                }
                match o.get("cursor") {
                    None | Some(serde_json::Value::Null) => {}
                    Some(c) => hex6(Some(c), &format!("term.customSchemes[{i}].cursor"))?,
                }
            }
            Ok(())
        }
        "term.fontSize" => check_range(&v, 8, 32, "term.fontSize"),
        "term.opacity" => check_range(&v, 0, 100, "term.opacity"),
        "term.scheme" => check_str(&v, TERM_SCHEME_IDS, "term.scheme"),
        "transfer.verifyAfterTransfer" => check_bool(&v, "transfer.verifyAfterTransfer"),
        "ui.copyOnSelect" | "ui.ctrlVPaste" | "ui.multilinePasteConfirm" | "ui.restoreUnclosed" => {
            check_bool(&v, key)
        }
        // 手动更新检查地址（M4b）。三条规矩，每条都对应一个具体的坏结局：
        //
        //  ① 空串合法且**保持为空串**——这是用户撤回「允许它联网」的唯一动作。若这里
        //     把空串判非法，清空输入框会报错、库里的旧地址原封不动，用户就再也关不掉了。
        //  ② 非空必须 https。检查结果里的发布页 URL 会被交给系统浏览器打开，明文 http
        //     的 manifest 可被中间人替换成钓鱼地址——我们不下载不安装，但「引导用户去哪儿
        //     下载」同样是可被劫持的一步。
        //  ③ 长度上限。设置库里的值会被完整读进内存并显示在输入框里。
        "update.manifestUrl" => {
            let s = v.as_str().ok_or("update.manifestUrl 必须是字符串")?;
            let s = s.trim();
            if s.is_empty() {
                return Ok(()); // ① 清空 = 关闭检查
            }
            if s.chars().count() > 2048 {
                return Err("update.manifestUrl 长度超过 2048".into());
            }
            if !s.starts_with("https://") {
                return Err(
                    "update.manifestUrl 必须以 https:// 开头（明文 http 可被中间人改写成钓鱼地址）"
                        .into(),
                );
            }
            Ok(())
        }
        "ui.density" => check_str(&v, &["compact", "loose"], "ui.density"),
        "ui.sidebarDock" => check_str(&v, &["left", "right", "float"], "ui.sidebarDock"),
        // 工具栏布局：两个字符串数组。条目数上限 64——工具栏按钮不到十个，
        // 上界只为挡「把整个库塞进一个键」。
        "ui.toolbarLayout" => {
            let o = v.as_object().ok_or("ui.toolbarLayout 必须是对象")?;
            for field in ["hidden", "order"] {
                let arr = o
                    .get(field)
                    .and_then(|x| x.as_array())
                    .ok_or_else(|| format!("ui.toolbarLayout.{field} 必须是数组"))?;
                if arr.len() > 64 {
                    return Err(format!(
                        "ui.toolbarLayout.{field} 条目数 {} 超过上限 64",
                        arr.len()
                    ));
                }
                for (i, item) in arr.iter().enumerate() {
                    let s = item
                        .as_str()
                        .ok_or_else(|| format!("ui.toolbarLayout.{field}[{i}] 必须是字符串"))?;
                    if s.is_empty() || s.chars().count() > 64 {
                        return Err(format!("ui.toolbarLayout.{field}[{i}] 长度须在 1..=64"));
                    }
                }
            }
            Ok(())
        }
        "ui.language" => check_str(&v, LANGUAGE_IDS, "ui.language"),
        "ui.rightClick" => check_str(&v, &["paste", "menu"], "ui.rightClick"),
        "sidebar.folders" => {
            let arr = v.as_array().ok_or("sidebar.folders 必须是数组")?;
            if arr.len() > 200 {
                return Err(format!("sidebar.folders 条目数 {} 超过上限 200", arr.len()));
            }
            for (i, item) in arr.iter().enumerate() {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("sidebar.folders[{i}] 必须是字符串"))?;
                if s.is_empty() || s.chars().count() > 512 {
                    return Err(format!("sidebar.folders[{i}] 长度须在 1..=512"));
                }
            }
            Ok(())
        }
        "sidebar.hostProbeSeconds" => check_range(&v, 0, 600, "sidebar.hostProbeSeconds"), // 0 = 关闭轮询（不探测任何主机）
        // 会话纯文本日志（M4a）：对象四字段。dir 复用沙箱根同款长度上限
        // （同是文件系统路径，产品边界只该有一条）；template 256 足够任何
        // 占位符组合，长模板只会造出难以管理的文件名。
        "session.log" => {
            let obj = v.as_object().ok_or("session.log 必须是对象")?;
            check_bool(
                obj.get("enabled").unwrap_or(&serde_json::Value::Null),
                "session.log.enabled",
            )?;
            check_bool(
                obj.get("append").unwrap_or(&serde_json::Value::Null),
                "session.log.append",
            )?;
            check_str_len(
                obj.get("dir").unwrap_or(&serde_json::Value::Null),
                SANDBOX_ROOT_CHARS_MAX,
                "session.log.dir",
            )?;
            check_str_len(
                obj.get("template").unwrap_or(&serde_json::Value::Null),
                256,
                "session.log.template",
            )
        }
        // RDP 帧投递路径（4d）：恰好两档，别的不收。
        "rdp.frameTransport" => {
            let s = v
                .as_str()
                .ok_or("rdp.frameTransport 必须是字符串（raw | event）")?;
            if s != "raw" && s != "event" {
                return Err(format!("rdp.frameTransport 只认 raw 或 event，实得 {s:?}"));
            }
            Ok(())
        }
        // 终端内传输（M4a）：对象两字段，都是 bool。
        "term.zmodem" => {
            let obj = v.as_object().ok_or("term.zmodem 必须是对象")?;
            check_bool(
                obj.get("enabled").unwrap_or(&serde_json::Value::Null),
                "term.zmodem.enabled",
            )?;
            check_bool(
                obj.get("autoReceive").unwrap_or(&serde_json::Value::Null),
                "term.zmodem.autoReceive",
            )
        }
        "ui.sidebarWidth" => {
            // 前端布局钳位 160..=480（layout.ts SIDEBAR_MIN/MAX），写侧按同包络收
            let n = v
                .as_u64()
                .map(|n| n as f64)
                .or_else(|| v.as_f64())
                .ok_or("ui.sidebarWidth 必须是数字")?;
            if !n.is_finite() || !(160.0..=480.0).contains(&n) {
                return Err(format!("ui.sidebarWidth 取值越界：{n}（允许 160..=480）"));
            }
            Ok(())
        }
        "ui.theme" => {
            let mut allowed = vec!["auto"];
            allowed.extend_from_slice(THEME_IDS);
            check_str(&v, &allowed, "ui.theme")
        }
        "vault.autoLockMinutes" => {
            // 两种写法都认（与读侧 parse_auto_lock_minutes 的历史兼容口径一致）：
            // `"5"`（前端 JSON.stringify("5")）与 `5`（旧版裸数字）。
            let n = v
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .or_else(|| v.as_u64())
                .ok_or("vault.autoLockMinutes 必须是数字或数字字符串")?;
            if !AUTO_LOCK_MINUTES.contains(&n) {
                return Err(format!(
                    "vault.autoLockMinutes 取值非法：{n}（界面仅提供 0/5/30）"
                ));
            }
            Ok(())
        }
        other => Err(format!("未知设置键：{other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 白名单上每一个键的最常见合法值：逐键过一遍，防 match 分支写坏。
    #[test]
    fn all_allowed_keys_accept_their_canonical_values() {
        let canon = [
            ("rdp.frameTransport", r#""event""#),
            ("hostkey.defaultPolicy", r#""strict""#),
            ("ai.mode", r#""with_confirm""#),
            (
                "ai.providers",
                r#"[{"id":"a1","kind":"ollama","base_url":"http://localhost:11434","model":"llama3.1","key_ref":null,"allow_screen_context":false}]"#,
            ),
            (
                "keyboard.bindings",
                r#"{"Ctrl+Alt+K":"edit.clearScreen","Ctrl+F":null}"#,
            ),
            ("keyboard.mode", r#""local""#),
            ("mcp.enabled", "true"),
            ("mcp.caller", r#""claude-desktop""#),
            ("mcp.tools", r#"["sessions.list","command.run"]"#),
            (
                "mcp.mounts",
                r#"[{"server_id":"fs","command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","/data"]}]"#,
            ),
            (
                "confirm.suppressed",
                r#"["process.kill","service.restart"]"#,
            ),
            (
                "quick.commands",
                r#"[{"id":"qc1","name":"磁盘","command":"df -h","category":"诊断"},{"id":"qc2","name":"重启 nginx","command":"systemctl restart nginx"}]"#,
            ),
            (
                "security.clipboardClear",
                r#"{"enabled":true,"seconds":60}"#,
            ),
            (
                "session.log",
                r#"{"enabled":true,"dir":"","template":"{host}.log","append":true}"#,
            ),
            ("sidebar.folders", r#"["工作","工作/生产"]"#),
            ("sidebar.hostProbeSeconds", "30"),
            ("sftp.sandboxRoot", r#""C:\\sbx""#),
            (
                // r## 而非 r#：色值里的 `#` 会提前终止 r#"…"# 字面量
                "term.highlights",
                r##"[{"id":"h1","name":"错误","pattern":"ERROR","kind":"literal","color":"#ff5555","alert":true,"enabled":true}]"##,
            ),
            ("term.zmodem", r#"{"enabled":true,"autoReceive":false}"#),
            (
                "term.customSchemes",
                r##"[{"name":"MyDark","foreground":"#cccccc","background":"#1e1e2e","ansi":["#45475a","#f38ba8","#a6e3a1","#f9e2af","#89b4fa","#f5c2e7","#94e2d5","#bac2de","#585b70","#f38ba8","#a6e3a1","#f9e2af","#89b4fa","#f5c2e7","#94e2d5","#a6adc8"],"cursor":"#f5e0dc"}]"##,
            ),
            ("term.bell", r#""badge""#),
            ("term.fontSize", "13"),
            ("term.opacity", "90"),
            ("term.scheme", r#""dracula""#),
            ("transfer.verifyAfterTransfer", "true"),
            ("ui.copyOnSelect", "false"),
            ("ui.ctrlVPaste", "true"),
            ("ui.density", r#""compact""#),
            ("ui.language", r#""en""#),
            ("ui.multilinePasteConfirm", "true"),
            ("ui.restoreUnclosed", "false"),
            ("ui.rightClick", r#""paste""#),
            ("ui.sidebarWidth", "240"),
            ("ui.sidebarDock", r#""right""#),
            (
                "ui.toolbarLayout",
                r#"{"hidden":["tools.ai"],"order":["tools.settings","edit.copy"]}"#,
            ),
            ("ui.theme", r#""slate""#),
            ("update.manifestUrl", r#""https://example.com/latest.json""#),
            ("vault.autoLockMinutes", r#""5""#),
        ];
        assert_eq!(
            canon.len(),
            ALLOWED_SETTING_KEYS.len(),
            "白名单与用例表必须逐键对齐"
        );
        for (k, val) in canon {
            assert!(validate_setting(k, val).is_ok(), "{k} 的合法值 {val} 被拒");
        }
    }

    /// 白名单外的键：拒绝（含注释掉的死配置）。
    #[test]
    fn unknown_keys_are_rejected() {
        for k in ["term.ghost", "anything.else", "ui.density "] {
            let err = validate_setting(k, "true").unwrap_err();
            assert!(err.contains("未知设置键"), "{k} → {err}");
        }
        // 键闸先于值解析：值烂成非 JSON 也要先报「未知设置键」而不是「不是合法 JSON」。
        // 顺序反了（白名单闸被拆、靠 match 的 catch-all 兜底）这两条路就分不出来了——
        // catch-all 的文案与白名单闸逐字相同，只有这条「坏值」用例能把层级钉死。
        let err = validate_setting("term.ghost", "not json").unwrap_err();
        assert!(err.contains("未知设置键"), "键闸应优先于值解析：{err}");
    }

    /// `keyboard.bindings`（M4a 键盘配置文件）的形状闸：对象 / 键位串包络 /
    /// 值为字符串或 null。规范形不在此校验（前端 keymap.ts 职责，见白名单注）。
    #[test]
    fn keyboard_bindings_shape_is_enforced() {
        assert!(
            validate_setting("keyboard.bindings", r#"{}"#).is_ok(),
            "空表合法（等价全默认）"
        );
        assert!(validate_setting("keyboard.bindings", r#"{"Ctrl+N":"session.new"}"#).is_ok());
        assert!(
            validate_setting("keyboard.bindings", r#"{"Ctrl+F":null}"#).is_ok(),
            "null = 显式解绑"
        );

        let err = validate_setting("keyboard.bindings", r#"[1]"#).unwrap_err();
        assert!(err.contains("必须是对象"), "{err}");
        let err = validate_setting("keyboard.bindings", r#"{"Ctrl+N":3}"#).unwrap_err();
        assert!(err.contains("必须是字符串或 null"), "{err}");
        let err = validate_setting("keyboard.bindings", r#"{"":"x"}"#).unwrap_err();
        assert!(err.contains("键位串"), "{err}");
        let long = format!(r#"{{"{}":"x"}}"#, "K".repeat(49));
        let err = validate_setting("keyboard.bindings", &long).unwrap_err();
        assert!(err.contains("1..=48"), "{err}");
    }

    /// `quick.commands`（M4a 快速命令集/片段库）的形状闸：每条违规都要点名字段，
    /// 否则删掉某一道字段检查测试照绿（「只认 is_err」的弱见证问题同上）。
    #[test]
    fn quick_commands_shape_is_enforced_per_field() {
        let ok = r#"[{"id":"q1","name":"磁盘","command":"df -h"}]"#;
        assert!(validate_setting("quick.commands", ok).is_ok());

        // 非数组
        let err = validate_setting("quick.commands", r#"{"a":1}"#).unwrap_err();
        assert!(err.contains("必须是数组"), "{err}");
        // 元素非对象
        let err = validate_setting("quick.commands", r#"["x"]"#).unwrap_err();
        assert!(err.contains("必须是对象"), "{err}");
        // name 缺失
        let err =
            validate_setting("quick.commands", r#"[{"id":"q1","command":"ls"}]"#).unwrap_err();
        assert!(err.contains("name 必须是字符串"), "{err}");
        // command 超长
        let long_cmd = format!(
            r#"[{{"id":"q1","name":"x","command":"{}"}}]"#,
            "c".repeat(2049)
        );
        let err = validate_setting("quick.commands", &long_cmd).unwrap_err();
        assert!(err.contains("command") && err.contains("2048"), "{err}");
        // id 重复
        let dup =
            r#"[{"id":"q1","name":"a","command":"ls"},{"id":"q1","name":"b","command":"ls"}]"#;
        let err = validate_setting("quick.commands", dup).unwrap_err();
        assert!(err.contains("id 重复"), "{err}");
        // category 非字符串
        let bad_cat = r#"[{"id":"q1","name":"a","command":"ls","category":3}]"#;
        let err = validate_setting("quick.commands", bad_cat).unwrap_err();
        assert!(err.contains("category 必须是字符串"), "{err}");
    }

    /// 键/值自身的规模上限。断言必须点名**是哪一道闸**响的：上限闸与各键的值规则
    /// 都会拒绝，「只认 is_err」会让闸被悄悄删掉而测试照绿（正是变异战役抓出来的弱见证）。
    #[test]
    fn key_and_value_size_limits() {
        let long_key = "k".repeat(SETTING_KEY_CHARS_MAX + 1);
        let err = validate_setting(&long_key, "true").unwrap_err();
        assert!(err.contains("键长度"), "键上限闸未响：{err}");
        let long_val = format!(r#""{}""#, "x".repeat(SETTING_VALUE_BYTES_MAX + 1));
        let err = validate_setting("ui.density", &long_val).unwrap_err();
        assert!(err.contains("字节上限"), "值上限闸未响：{err}");
    }

    /// 枚举类键的非法取值。
    #[test]
    fn enum_keys_reject_off_list_values() {
        for (k, bad) in [
            ("hostkey.defaultPolicy", r#""fingerprint""#),
            ("keyboard.mode", r#""both""#),
            ("term.bell", r#""badge+notify""#),
            ("ui.density", r#""cozy""#),
            ("ui.rightClick", r#""none""#),
            ("term.scheme", r#""not-a-scheme""#),
            ("ui.theme", r#""pink""#),
        ] {
            assert!(validate_setting(k, bad).is_err(), "{k} = {bad} 应被拒");
        }
    }

    /// 数字键的越界与类型错误。
    #[test]
    fn numeric_keys_reject_out_of_range() {
        for (k, bad) in [
            ("term.fontSize", "7"),
            ("term.fontSize", "33"),
            ("term.fontSize", r#""13""#), // 字符串而非数字
            ("term.opacity", "101"),
            ("ui.sidebarWidth", "159"),
            ("ui.sidebarWidth", "481"),
            ("ui.sidebarWidth", r#""240""#),
            ("vault.autoLockMinutes", "7"),
            ("vault.autoLockMinutes", r#""7""#),
        ] {
            assert!(validate_setting(k, bad).is_err(), "{k} = {bad} 应被拒");
        }
    }

    /// clipboardClear 的结构规则：enabled 必布尔、seconds 必在 [1,3600]。
    #[test]
    fn clipboard_clear_structure_rules() {
        // 合法边界：1 秒与 3600 秒都收
        assert!(validate_setting(
            "security.clipboardClear",
            r#"{"enabled":false,"seconds":1}"#
        )
        .is_ok());
        assert!(validate_setting(
            "security.clipboardClear",
            r#"{"enabled":true,"seconds":3600}"#
        )
        .is_ok());
        for bad in [
            r#"{"seconds":60}"#,                  // 缺 enabled
            r#"{"enabled":1,"seconds":60}"#,      // enabled 非布尔
            r#"{"enabled":true,"seconds":0}"#,    // 0 秒 = 复制即清
            r#"{"enabled":true,"seconds":3601}"#, // 越上界（钳位语义只属于读侧）
            r#"{"enabled":true,"seconds":"60"}"#, // 字符串
            r#"[]"#,                              // 非对象
        ] {
            assert!(
                validate_setting("security.clipboardClear", bad).is_err(),
                "{bad} 应被拒"
            );
        }
    }

    /// 布尔键只认真布尔。
    #[test]
    fn bool_keys_reject_non_bool() {
        for k in [
            "transfer.verifyAfterTransfer",
            "ui.copyOnSelect",
            "ui.ctrlVPaste",
            "ui.multilinePasteConfirm",
            "ui.restoreUnclosed",
        ] {
            for bad in [r#""true""#, "1", "null"] {
                assert!(validate_setting(k, bad).is_err(), "{k} = {bad} 应被拒");
            }
        }
    }

    /// 沙箱路径的长度上限。坏例同样点名是**路径规则**响的（而非值字节闸抢先）——
    /// 两道闸挨得很近，只认 is_err 会让「路径规则被删、字节闸替它兜底」的漂移照绿。
    #[test]
    fn sandbox_root_length_limit() {
        let ok = format!(r#""{}""#, "x".repeat(SANDBOX_ROOT_CHARS_MAX));
        assert!(validate_setting("sftp.sandboxRoot", &ok).is_ok());
        let bad = format!(r#""{}""#, "x".repeat(SANDBOX_ROOT_CHARS_MAX + 1));
        let err = validate_setting("sftp.sandboxRoot", &bad).unwrap_err();
        assert!(err.contains("长度超过上限"), "路径长度闸未响：{err}");
    }

    /// vault.autoLockMinutes 的两种历史写法（JSON 串与裸数字）都收，但只收 0/5/30。
    #[test]
    fn auto_lock_accepts_both_wire_forms() {
        for (val, n) in [
            (r#""0""#, 0),
            (r#""5""#, 5),
            (r#""30""#, 30),
            ("0", 0),
            ("30", 30),
        ] {
            assert!(
                validate_setting("vault.autoLockMinutes", val).is_ok(),
                "{val} 应被收"
            );
            assert!(AUTO_LOCK_MINUTES.contains(&n));
        }
    }

    /// 非 JSON 值整体被拒。
    #[test]
    fn non_json_values_are_rejected() {
        for bad in ["not-json", "{broken", "truex"] {
            assert!(validate_setting("ui.density", bad).is_err(), "{bad} 应被拒");
        }
    }

    /// **API key 不许写进 settings。**
    ///
    /// settings 表是明文 JSON，而「导出设置」「贴日志求助」是用户每天都在做的两件事。
    /// 唯一允许的密钥相关字段是 `key_ref`（一个 Vault 记录 id）。
    ///
    /// 这道闸挡的是**将来的自己**：哪天有人图省事在 provider 条目里加一个 api_key
    /// 字段，写入就会被拒——而那比事后在用户贴出来的日志里发现 key 好得多。
    #[test]
    fn ai_providers_rejects_any_secret_shaped_field() {
        let base = |extra: &str| {
            format!(r#"[{{"id":"a","kind":"ollama","base_url":"http://x","model":"m"{extra}}}]"#)
        };
        // 反向对照：不带那些字段的版本是合法的（否则下面几条会因为整体非法而假绿）
        assert!(validate_setting("ai.providers", &base("")).is_ok());
        assert!(validate_setting("ai.providers", &base(r#","key_ref":7"#)).is_ok());

        for banned in [
            r#","api_key":"sk-xxx""#,
            r#","apiKey":"sk-xxx""#,
            r#","key":"sk-xxx""#,
            r#","secret":"s""#,
            r#","token":"t""#,
            r#","password":"p""#,
            r#","passphrase":"p""#,
        ] {
            let v = base(banned);
            let e = validate_setting("ai.providers", &v).expect_err(&format!("{banned} 本该被拒"));
            assert!(e.contains("key_ref"), "错误里要指出该放 key_ref：{e}");
        }
    }

    #[test]
    fn ai_providers_checks_shape_and_envelope() {
        // kind 必须在三家之内——认不出的 kind 会让请求体用错协议
        assert!(validate_setting(
            "ai.providers",
            r#"[{"id":"a","kind":"gemini","base_url":"http://x","model":"m"}]"#
        )
        .is_err());
        // base_url 必须 http(s)：file:// 会让 reqwest 报一句与配置无关的错
        for bad_url in ["file:///etc/passwd", "ftp://x", "localhost:11434", ""] {
            let v = format!(r#"[{{"id":"a","kind":"ollama","base_url":"{bad_url}","model":"m"}}]"#);
            assert!(
                validate_setting("ai.providers", &v).is_err(),
                "{bad_url} 本该被拒"
            );
        }
        // 四个必填字段各自缺一个都要红
        for missing in ["id", "kind", "base_url", "model"] {
            let mut o = serde_json::json!({
                "id":"a","kind":"ollama","base_url":"http://x","model":"m"
            });
            o.as_object_mut().unwrap().remove(missing);
            let v = serde_json::to_string(&serde_json::json!([o])).unwrap();
            assert!(
                validate_setting("ai.providers", &v).is_err(),
                "缺 {missing} 本该被拒"
            );
        }
        // allow_screen_context 必须是布尔（字符串 "true" 不算）
        assert!(validate_setting(
            "ai.providers",
            r#"[{"id":"a","kind":"ollama","base_url":"http://x","model":"m","allow_screen_context":"true"}]"#
        )
        .is_err());
    }

    /// 档位串必须是**下划线版**（与 `fs_policy::AiMode::as_str()` 同源）。
    ///
    /// 连字符版被 `from_str_exact` 认不出、回落 Disabled——症状是
    /// 「选了只读，AI 却整个不能用」，而设置页上那一项看着是选中的。
    #[test]
    fn ai_mode_accepts_only_the_underscore_form() {
        for ok in ["disabled", "read_only", "with_confirm"] {
            assert!(
                validate_setting("ai.mode", &format!("\"{ok}\"")).is_ok(),
                "{ok} 本该被接受"
            );
        }
        for bad in ["read-only", "with-confirm", "readonly", "off", ""] {
            assert!(
                validate_setting("ai.mode", &format!("\"{bad}\"")).is_err(),
                "{bad} 本该被拒"
            );
        }
    }

    /// 自定义配色：颜色串只认 `#rrggbb`。
    ///
    /// 这个键的值有一部分来自**外来文件**（导入 Xshell `.xcs` / iTerm2 `.itermcolors`），
    /// 而这些颜色串最终会进 CSS 变量与终端主题对象。一个能穿过校验的畸形串在那里
    /// 就是一段注入 style 的文本，所以这条比别的形状校验更该严：不认 `rgb()`、
    /// 不认颜色名、不认 8 位带 alpha、不认少了 `#` 的裸十六进制。
    #[test]
    fn custom_schemes_only_accept_hash_rrggbb() {
        let mk = |c: &str| {
            let ansi: Vec<String> = (0..16).map(|_| "#112233".to_string()).collect();
            serde_json::json!([{
                "name": "X", "foreground": c, "background": "#000000", "ansi": ansi
            }])
            .to_string()
        };
        assert!(validate_setting("term.customSchemes", &mk("#aabbcc")).is_ok());
        for bad in [
            // 大写：前端 HEX6 是 `/^#[0-9a-f]{6}$/`，大写串会被它静默过滤掉。
            // 后端宽容 + 前端严格 = 导入报告说成功、设置页里什么都没多出来，中间无一处报错。
            "#AABBCC",
            "aabbcc",    // 没有 #
            "#aabbccdd", // 带 alpha
            "#abc",      // 三位简写
            "rgb(1,2,3)",
            "red",
            "#gggggg", // 不是十六进制
            "#aabbc",  // 少一位
            "javascript:alert(1)",
            "red; background:url(x)", // 注入形状
        ] {
            assert!(
                validate_setting("term.customSchemes", &mk(bad)).is_err(),
                "{bad} 本该被拒"
            );
        }
        // ansi 必须**恰好** 16 项：少一项会让某个色号取到 undefined，多一项说明解析出了别的东西
        for n in [0usize, 8, 15, 17] {
            let ansi: Vec<String> = (0..n).map(|_| "#112233".to_string()).collect();
            let v = serde_json::json!([{
                "name": "X", "foreground": "#ffffff", "background": "#000000", "ansi": ansi
            }])
            .to_string();
            assert!(
                validate_setting("term.customSchemes", &v).is_err(),
                "ansi {n} 项本该被拒"
            );
        }
        // cursor 可缺省、可为 null，但给了就要合法
        let with_cursor = |c: serde_json::Value| {
            let ansi: Vec<String> = (0..16).map(|_| "#112233".to_string()).collect();
            serde_json::json!([{
                "name": "X", "foreground": "#ffffff", "background": "#000000",
                "ansi": ansi, "cursor": c
            }])
            .to_string()
        };
        assert!(
            validate_setting("term.customSchemes", &with_cursor(serde_json::Value::Null)).is_ok()
        );
        assert!(validate_setting("term.customSchemes", &with_cursor("#ff0000".into())).is_ok());
        assert!(validate_setting("term.customSchemes", &with_cursor("nope".into())).is_err());
    }

    /// 更新检查地址：空串必须**存得进去**，非 https 必须存不进去。
    ///
    /// 这两条方向相反，且都是安全性质的：
    ///  · 空串存不进去 ⇒ 用户清空输入框时静默失败，库里的旧地址还在，
    ///    「我已经关掉更新检查了」这个认知与事实不符。这是唯一的关闭动作。
    ///  · 明文 http 存得进去 ⇒ 清单可被中间人整份替换，检查结果里那个
    ///    「打开发布页」按钮就指向攻击者的页面。
    #[test]
    fn the_update_url_accepts_empty_and_https_only() {
        for ok in [
            r#""""#,    // 清空 = 关闭检查
            r#""   ""#, // 只有空白，等同清空
            r#""https://example.com/latest.json""#,
            r#""https://example.com/a?b=c#d""#,
        ] {
            assert!(
                validate_setting("update.manifestUrl", ok).is_ok(),
                "{ok} 本该被接受"
            );
        }
        for bad in [
            r#""http://example.com/latest.json""#, // 明文
            r#""file:///C:/x.json""#,
            r#""javascript:alert(1)""#,
            r#""//example.com/latest.json""#,
            r#""example.com/latest.json""#,
            "123",  // 不是字符串
            "true", // 不是字符串
            "null", // 不是字符串
        ] {
            assert!(
                validate_setting("update.manifestUrl", bad).is_err(),
                "{bad} 本该被拒"
            );
        }
        // 长度上限：2048 过，2049 不过（值是 JSON 字符串，故要带引号计算）
        let head = "https://e.com/";
        let ok = format!("{head}{}", "a".repeat(2048 - head.len()));
        assert!(validate_setting("update.manifestUrl", &format!("\"{ok}\"")).is_ok());
        assert!(validate_setting("update.manifestUrl", &format!("\"{ok}a\"")).is_err());
    }

    // ───────────── 跨语言守卫（审计2 #40） ─────────────

    /// 递归收集 `frontend/src` 下的 .ts/.svelte（排除 *.test.ts——测试文件里的
    /// 代码串/注释串会污染提取，前端自己的 settings-wiring.test.ts 已单独钉住 test 侧）。
    fn frontend_sources() -> Vec<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../frontend/src")
            .canonicalize()
            .expect("找不到 frontend/src（app crate 的兄弟目录）");
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).expect("read_dir") {
                let p = e.expect("entry").path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if (name.ends_with(".ts") || name.ends_with(".svelte"))
                        && !name.ends_with(".test.ts")
                    {
                        out.push(std::fs::read_to_string(&p).expect("read source"));
                    }
                }
            }
        }
        out
    }

    /// 从源码提取 `settingSet("字面量键", …)` 的全部键（变量形态另行断言，见下）。
    fn extract_literal_setting_keys(texts: &[String]) -> Vec<String> {
        let mut keys = Vec::new();
        for t in texts {
            let mut rest: &str = t;
            while let Some(i) = rest.find("settingSet(") {
                rest = &rest[i + "settingSet(".len()..];
                let arg = rest.trim_start();
                if let Some(after) = arg.strip_prefix('"') {
                    if let Some(end) = after.find('"') {
                        keys.push(after[..end].to_string());
                    }
                }
            }
        }
        keys
    }

    /// 前端每一处 `settingSet("key")` 的字面量键都必须在白名单上。
    /// 「前端加了控件、后端忘了登记」的漂移在这里**编译期外**的第一道闸上变红——
    /// 这正是审计2 #40 要修的「泛型只做 TypeScript 断言」的自动载体。
    #[test]
    fn every_frontend_setting_key_is_whitelisted() {
        let texts = frontend_sources();
        let keys = extract_literal_setting_keys(&texts);
        assert!(!keys.is_empty(), "一个键都提取不到 = 扫描本身坏了");
        for k in keys {
            assert!(
                ALLOWED_SETTING_KEYS.contains(&k.as_str()),
                "前端 settingSet(\"{k}\") 不在 settings_set 白名单上——要么补白名单与值规则，要么删控件"
            );
        }
    }

    /// 变量形态的写入键（`settingSet(THEME_SETTING_KEY, …)`）：其**定义处**的字面量同样
    /// 必须在白名单上。定义走 `XXX_KEY = "…"` 形式，逐文件提取。
    #[test]
    fn frontend_key_constants_are_whitelisted() {
        let mut found = 0;
        for t in &frontend_sources() {
            let mut rest: &str = t;
            while let Some(i) = rest.find("_KEY = \"") {
                let after = &rest[i + "_KEY = \"".len()..];
                // 引号不闭合：格式坏到不值得写状态机，跳过本文件剩余部分。
                // 游标必须推进到闭合引号之后——不推进就是死循环（上一版就在这里把 found 加到溢出）。
                let Some(end) = after.find('"') else { break };
                let k = &after[..end];
                assert!(
                    ALLOWED_SETTING_KEYS.contains(&k),
                    "前端设置键常量 {k} 不在白名单上"
                );
                found += 1;
                rest = &after[end + 1..];
            }
        }
        assert!(found > 0, "键常量定义提取失败（扫描坏了）");
    }

    /// `term.scheme` 白名单与前端内置方案双向同步：.json 的 id 集 == Rust 表。
    /// 单侧断言（每个 id 都在表上）挡不住「.json 加了新方案、Rust 忘了加」——
    /// 新方案会被写侧拒绝；双侧断言连方向也钉住：Rust 表里也不许有 .json 没有的幽灵 id。
    #[test]
    fn term_scheme_ids_match_frontend_builtins() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../frontend/src/lib/term-schemes")
            .canonicalize()
            .expect("找不到 term-schemes 目录");
        let mut ids = Vec::new();
        for e in std::fs::read_dir(dir).expect("read_dir") {
            let p = e.expect("entry").path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let t = std::fs::read_to_string(&p).expect("read scheme json");
            let i = t.find("\"id\"").expect("scheme json 缺 id");
            let rest = &t[i + 4..];
            let after = rest.find('"').expect("id 引号不闭合");
            let rest2 = &rest[after + 1..];
            let end = rest2.find('"').expect("id 引号不闭合");
            ids.push(rest2[..end].to_string());
        }
        ids.sort();
        let mut rust: Vec<&str> = TERM_SCHEME_IDS.to_vec();
        rust.sort();
        assert_eq!(
            ids, rust,
            "term.scheme 白名单与 term-schemes/*.json 的 id 集必须逐项相等"
        );
    }

    /// `ui.theme` 白名单与前端主题表双向同步（themes.ts 的 `id: "…"` 字面量集）。
    #[test]
    fn theme_ids_match_frontend_themes() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../frontend/src/lib/theme/themes.ts")
            .canonicalize()
            .expect("找不到 themes.ts");
        let t = std::fs::read_to_string(path).expect("read themes.ts");
        let mut ids = Vec::new();
        let mut rest: &str = &t;
        while let Some(i) = rest.find("id: \"") {
            rest = &rest[i + 5..];
            if let Some(end) = rest.find('"') {
                ids.push(rest[..end].to_string());
            }
        }
        ids.sort();
        let mut rust: Vec<&str> = THEME_IDS.to_vec();
        rust.sort();
        assert_eq!(
            ids, rust,
            "ui.theme 白名单与 themes.ts 的 id 集必须逐项相等"
        );
    }

    /// `ui.language` 白名单与前端 `SUPPORTED_LOCALES` 双向同步（M4b i18n）。
    ///
    /// 两边走散的后果不是崩溃，是一个说不通的错误：用户从我们自己给的下拉框里选了一项，
    /// 后端回他「ui.language 取值非法」。而这种错误只在**加第三种语言**时出现——
    /// 那时谁也不会想到还有个 Rust 常量要跟着改。
    #[test]
    fn language_ids_match_frontend_supported_locales() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../frontend/src/lib/i18n/index.ts")
            .canonicalize()
            .expect("找不到 i18n/index.ts");
        let src = std::fs::read_to_string(path).expect("read i18n/index.ts");
        // 只取 SUPPORTED_LOCALES 那个数组字面量的范围内的 `id: "…"`，
        // 否则文件里别处的 `id:` 会混进来（本文件目前没有，但守卫不该依赖这一点）。
        let start = src
            .find("SUPPORTED_LOCALES")
            .expect("i18n/index.ts 里找不到 SUPPORTED_LOCALES —— 守卫已失效");
        let tail = &src[start..];
        let end = tail
            .find("] as const;")
            .expect("SUPPORTED_LOCALES 的数组没有闭合标记");
        let block = &tail[..end];

        let mut ids = Vec::new();
        let mut rest: &str = block;
        while let Some(i) = rest.find("id: \"") {
            rest = &rest[i + 5..];
            if let Some(e) = rest.find('"') {
                ids.push(rest[..e].to_string());
            }
        }
        // 抓不到任何 id 时空集会与空集相等——那是假绿。下限钉死在「至少两种」，
        // 因为「语言切换」这个功能只有一种语言时根本不成立。
        assert!(
            ids.len() >= 2,
            "只从 SUPPORTED_LOCALES 抓到 {} 个 id，提取逻辑多半已失效：{block}",
            ids.len()
        );
        ids.sort();
        let mut rust: Vec<&str> = LANGUAGE_IDS.to_vec();
        rust.sort();
        assert_eq!(
            ids, rust,
            "ui.language 白名单与 SUPPORTED_LOCALES 必须逐项相等"
        );
    }
}
