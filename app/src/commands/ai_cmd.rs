//! AI 面板的 IPC 出口（M2 出口第 7、8、12、13、14、16 项的接线层）。
//!
//! `crates/ai` 与 `crates/policy` 的能力早就齐了（provider 校验、三家报文、脱敏、
//! 上下文组装、策略分级、确认闸门），M2 一直卡在**没人把它们接到界面上**。
//! 本文件就是那条接线。
//!
//! # 「策略预判」是我们算的，不是模型说的
//!
//! 出口原文要求「生成结果必须带**解释**与**策略预判**两段」。解释来自模型，
//! 而策略预判**必须**由 `fs_policy::classify` 算出来——让模型自评自己建议的命令
//! 有多危险，等于让被审查者写审查报告。
//!
//! 总设计 §8.1 把 LLM 自评定为「仅辅票」，本文件连辅票都不取：模型只回命令与解释，
//! 危险度一律现算。少一个可以被提示注入影响的判据。
//!
//! # API key 不落 settings
//!
//! `settings` 表是明文 JSON。key 存 Vault（`SecretKind::ApiKey`），settings 里只留
//! 一个 Vault 记录 id。于是「导出设置」「贴日志求助」都不会把 key 带出去——
//! 而那两件事用户每天都在做。

use crate::state::AppState;
use fs_ai::provider::{ProviderConfig, ProviderKind, ProviderState};
use fs_ai::wire::{self, ChatRequest};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;

/// provider 配置存放的 settings 键（**不含 key**，见模块头）。
pub const PROVIDERS_KEY: &str = "ai.providers";
/// AI 执行档位（总设计 §4.3 总开关）：`disabled` / `read-only` / `with-confirm`。
pub const AI_MODE_KEY: &str = "ai.mode";

/// 一条 provider 配置在前端/settings 里的形态。
///
/// 与 `fs_ai::ProviderConfig` 分开是因为多了两件事：一个稳定 `id`（前端列表要 key，
/// 删改要定位）与 `key_ref`（Vault 记录 id，而不是 key 本身）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub id: String,
    /// `anthropic` / `openai` / `ollama`。
    pub kind: String,
    pub base_url: String,
    pub model: String,
    /// Vault 里的 API key 记录 id。Ollama 通常没有。
    #[serde(default)]
    pub key_ref: Option<u64>,
    /// §4.2 的按 provider 开关：允许把屏幕内容发给**这一个** provider 吗。
    ///
    /// `#[serde(default)]` 给出 `false`——忘写这个字段时的行为必须是「不发」。
    #[serde(default)]
    pub allow_screen_context: bool,
}

/// `kind` 串 → 枚举。**认不出即 `None`，绝不回落某个默认值**。
///
/// 回落的后果很具体：一个拼错的 kind（前端改名、手工改库）会被当成另一家的协议，
/// 于是请求体是 Anthropic 的形状、发到 OpenAI 的地址——用户看到的是一句 400，
/// 而真正的问题是那个串拼错了。
fn parse_kind(s: &str) -> Option<ProviderKind> {
    match s {
        "anthropic" => Some(ProviderKind::Anthropic),
        "openai" => Some(ProviderKind::OpenAiCompatible),
        "ollama" => Some(ProviderKind::Ollama),
        _ => None,
    }
}

fn kind_str(k: ProviderKind) -> &'static str {
    match k {
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::OpenAiCompatible => "openai",
        ProviderKind::Ollama => "ollama",
    }
}

/// 给前端的状态摘要。**不含 key，也不含 key_ref**。
///
/// 连 `key_ref` 都不给：前端拿它没有用处（保存时由用户重新输入 key），
/// 而一个记录 id 加上 `vault_get_secret` 就是一条取 key 的路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AiStatus {
    /// 有没有**校验通过**的 provider。`false` ⇒ 前端显示配置向导。
    pub configured: bool,
    /// 当前生效的那一个（`pick` 选出的第一个校验通过的）的摘要。
    pub active: Option<ActiveProvider>,
    /// 全部已保存的条目（供列表与编辑）。
    pub entries: Vec<ProviderSummary>,
    /// AI 执行档位。
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActiveProvider {
    pub kind: String,
    pub base_url: String,
    pub model: String,
    pub allow_screen_context: bool,
}

/// 列表里的一条。`has_key` 是**布尔**，不是 key 本身也不是 key_ref。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderSummary {
    pub id: String,
    pub kind: String,
    pub base_url: String,
    pub model: String,
    pub has_key: bool,
    pub allow_screen_context: bool,
    /// 校验不通过的原因（一句给人看的话）。通过时为 `None`。
    ///
    /// 出口原文：「未配置任何模型源时 AI 面板显示配置向导，**不崩溃、不降级为假数据**」。
    /// 一份填了一半的配置（填了 key 忘了模型名）必须显示成「这条还没填完」，
    /// 而不是安静地不生效——后者会让用户以为配好了，直到发请求时才报错。
    pub problem: Option<String>,
}

/// 读出全部 provider 条目。脏值一律回落空列表。
async fn load_entries(state: &AppState) -> Vec<ProviderEntry> {
    let raw = fs_connmgr::SettingsRepo::new(state.db.pool())
        .get(PROVIDERS_KEY)
        .await
        .ok()
        .flatten();
    raw.and_then(|s| serde_json::from_str::<Vec<ProviderEntry>>(&s).ok())
        .unwrap_or_default()
}

/// 把条目转成引擎侧的配置。`api_key` 从 Vault 取。
///
/// Vault 锁着时 key 取不到 —— 这时**不把它当成「没有 key」**，而是让这条配置
/// 带着空 key 去校验：Anthropic/OpenAI 会因缺 key 而校验失败，于是界面显示
/// 「这条还没填完」而不是「配好了但一发就 401」。
async fn to_config(state: &AppState, e: &ProviderEntry) -> Option<ProviderConfig> {
    let kind = parse_kind(&e.kind)?;
    let api_key = match e.key_ref {
        None => None,
        Some(id) => {
            let guard = state.vault.lock().await;
            guard
                .as_ref()
                .and_then(|s| s.get(id).ok())
                .and_then(|b| String::from_utf8(b.to_vec()).ok())
        }
    };
    Some(ProviderConfig {
        kind,
        base_url: e.base_url.clone(),
        model: e.model.clone(),
        api_key,
        allow_screen_context: e.allow_screen_context,
    })
}

/// AI 面板的状态。**这是面板打开时唯一要发的请求**——出口有「面板打开 ≤200ms」的指标，
/// 而每多一次 IPC 往返就多一次调度延迟。
#[tauri::command]
pub async fn ai_status(state: State<'_, Arc<AppState>>) -> Result<AiStatus, String> {
    let entries = load_entries(&state).await;
    let mut summaries = Vec::with_capacity(entries.len());
    let mut configs = Vec::with_capacity(entries.len());
    for e in &entries {
        let cfg = to_config(&state, e).await;
        let problem = match &cfg {
            None => Some(format!("认不出的 provider 类型：{}", e.kind)),
            Some(c) => c.validate().err().map(|err| err.message().to_string()),
        };
        summaries.push(ProviderSummary {
            id: e.id.clone(),
            kind: e.kind.clone(),
            base_url: e.base_url.clone(),
            model: e.model.clone(),
            has_key: e.key_ref.is_some(),
            allow_screen_context: e.allow_screen_context,
            problem,
        });
        if let Some(c) = cfg {
            configs.push(c);
        }
    }
    let state_pick = ProviderState::pick(&configs);
    let mode = fs_connmgr::SettingsRepo::new(state.db.pool())
        .get(AI_MODE_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<String>(&s).ok())
        // 默认 `with-confirm`：不是 `disabled`（那会让功能默认不存在，用户配好了还得再找个开关），
        // 也不是 `read-only`（那会让写命令永远被拒，而 NL→命令的主要用途就是写命令）。
        // 「每次执行都问一下」是唯一既可用又不会替用户做决定的默认值。
        .unwrap_or_else(|| "with-confirm".to_string());

    Ok(AiStatus {
        configured: state_pick.is_configured(),
        active: state_pick.config().map(|c| ActiveProvider {
            kind: kind_str(c.kind).to_string(),
            base_url: c.base_url.clone(),
            model: c.model.clone(),
            allow_screen_context: c.allow_screen_context,
        }),
        entries: summaries,
        mode,
    })
}

/* ─────────────────────── provider 增删（M2 出口第 7 项）─────────────────────── */

/// 保存一条 provider 配置。
///
/// # key 走 Vault，不走 settings
///
/// `api_key` 非空时存进 Vault（`SecretKind::ApiKey`），settings 里只留记录 id。
/// 空串表示「不改 key」——编辑既有条目时不必重输，而对 Ollama 来说空本来就是正常的。
///
/// # 同一个 (kind, base_url, model) 视为同一条
///
/// 用这三项做身份而不是让前端生成 id：用户重复填一次同样的配置时，
/// 期望是「改掉那一条」而不是「多出一条一模一样的」。而 id 由前端给的话，
/// 两次填写会拿到两个 id，列表里就出现两条看起来完全相同、删了一条还剩一条的条目。
#[tauri::command]
pub async fn ai_provider_save(
    kind: String,
    base_url: String,
    model: String,
    api_key: String,
    allow_screen_context: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if parse_kind(&kind).is_none() {
        return Err(format!("认不出的 provider 类型：{kind}"));
    }
    let base_url = base_url.trim().to_string();
    let model = model.trim().to_string();
    if base_url.is_empty() {
        // 校验放在这里而不是只靠 ai_status 的 problem：存一条明知无效的配置进去
        // 只是把错误推迟到列表里，而用户此刻正看着这个表单。
        return Err("请填写服务地址（本程序不内置任何默认地址）".into());
    }
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return Err("服务地址要以 http:// 或 https:// 开头".into());
    }
    if model.is_empty() {
        return Err("请填写模型名".into());
    }

    let mut entries = load_entries(&state).await;
    let identity = |e: &ProviderEntry| e.kind == kind && e.base_url == base_url && e.model == model;
    let existing_key_ref = entries.iter().find(|e| identity(e)).and_then(|e| e.key_ref);

    // key 非空 → 存进 Vault。空 → 沿用原来的（编辑时不重输）。
    let key_ref = if api_key.trim().is_empty() {
        existing_key_ref
    } else {
        let mut guard = state.vault.lock().await;
        let store = guard
            .as_mut()
            .ok_or("Vault 还锁着——API key 要存进 Vault，先解锁它")?;
        let label = format!("AI: {kind} {model}");
        Some(
            store
                // Zeroizing：key 的明文副本随作用域结束擦除。`put` 按值收它
                // 正是为了让调用方无法保留一份不擦除的拷贝。
                .put(
                    fs_vault::SecretKind::ApiKey,
                    label,
                    zeroize::Zeroizing::new(api_key.trim().as_bytes().to_vec()),
                )
                .map_err(|e| e.to_string())?,
        )
    };

    let entry = ProviderEntry {
        // id 由 (kind, base_url, model) 派生，稳定且可重算——前端不必记它。
        id: provider_id(&kind, &base_url, &model),
        kind: kind.clone(),
        base_url: base_url.clone(),
        model: model.clone(),
        key_ref,
        allow_screen_context,
    };
    match entries.iter().position(identity) {
        Some(i) => entries[i] = entry,
        None => entries.push(entry),
    }
    save_entries(&state, &entries).await
}

/// 删除一条 provider 配置。
///
/// **不删 Vault 里的 key**：那条记录可能被别的条目引用（同一把 key 配两个模型），
/// 而且用户删掉一个 provider 配置的意思是「不用这个源了」，不是「注销这个 key」。
/// Vault 里的清理归 Vault 管理器——那里能看到自己在删什么。
#[tauri::command]
pub async fn ai_provider_delete(id: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut entries = load_entries(&state).await;
    let before = entries.len();
    entries.retain(|e| e.id != id);
    if entries.len() == before {
        // 删一个不存在的条目不报错：用户可能在两个窗口里各删了一次，
        // 而第二次报「找不到」只会让他以为出了问题。
        return Ok(());
    }
    save_entries(&state, &entries).await
}

/// 条目 id：从身份三项派生。
///
/// 用可重算的派生值而不是随机 uuid，是为了让「同一份配置」在任何时候都得到同一个 id——
/// 于是前端删除时不必先查一遍列表拿 id，而重复保存也不会产出两条。
fn provider_id(kind: &str, base_url: &str, model: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    // 分隔符用 \u{1}：它不可能出现在这三项里，所以 ("a","bc") 与 ("ab","c")
    // 不会撞同一个 id。用 ":" 拼的话 URL 里的冒号就会造出这种碰撞。
    h.update(kind.as_bytes());
    h.update([1]);
    h.update(base_url.as_bytes());
    h.update([1]);
    h.update(model.as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
}

/// 写回 settings。**走 `validate_setting` 同一道校验**，不直接 repo.set——
/// 与 `import_cmd` 落库自定义配色是同一条理由：绕过校验的那条路径迟早会写进
/// 一份形状不对的值，而读侧只能选择容忍它。
async fn save_entries(state: &AppState, entries: &[ProviderEntry]) -> Result<(), String> {
    let json = serde_json::to_string(entries).map_err(|e| e.to_string())?;
    crate::commands::settings_cmd::validate_setting(PROVIDERS_KEY, &json)?;
    fs_connmgr::SettingsRepo::new(state.db.pool())
        .set(PROVIDERS_KEY, &json)
        .await
        .map_err(|e| e.to_string())
}

/* ─────────────────────── 提示模板（M2 出口第 8 项）─────────────────────── */

/// NL→命令的 system 提示。
///
/// # 为什么要求模型只回一行命令 + 一段解释，而不让它自评危险度
///
/// 出口原文要「生成结果必须带**解释**与**策略预判**两段」。解释来自模型；
/// 策略预判由 `fs_policy::classify` 现算——让模型评价自己建议的命令有多危险，
/// 等于让被审查者写审查报告。总设计 §8.1 把 LLM 自评定为「仅辅票」，
/// 这里连辅票都不取：少一个能被提示注入影响的判据。
///
/// # 为什么禁止 Markdown 代码围栏
///
/// 模型很爱把命令包在 ```bash 里。那三个反引号会被原样填进输入框，
/// 用户按回车执行的就是一个语法错误。要求纯文本比在解析侧剥围栏可靠——
/// 剥围栏得处理各种变体（```sh、``` 无语言、缩进四格），而漏掉一种就是一次失败执行。
const NL_TO_CMD_SYSTEM: &str = "\
你是一个 SSH 终端的命令助手。用户用自然语言描述意图，你给出可执行的 shell 命令。

严格按下面的格式回答，不要有任何其他内容：

COMMAND: <一行 shell 命令>
WHY: <一到三句话，说明这条命令做什么、为什么这么写>

规则：
- COMMAND 必须是**一行纯文本**，不要用 Markdown 代码围栏（``` ）包裹。
- 不要在 COMMAND 里写多条命令的组合，除非用户明确要求。
- 不要评估这条命令的危险程度——那由程序自己判定，你说了不算。
- 拿不准时在 WHY 里说明你的假设，不要凭空猜一个路径或主机名。
- 用中文写 WHY。";

/// 解读输出的 system 提示（M2 出口第 12 项）。
///
/// 输出**允许** Markdown（前端会做净化渲染），因为解读天然需要列表与代码片段。
/// 与 NL→命令那条的差别正在这里：那一条的产物要进输入框执行，Markdown 是污染；
/// 这一条的产物只给人读，Markdown 是可读性。
const EXPLAIN_SYSTEM: &str = "\
你是一个 SSH 终端的输出解读助手。用户会给你一段终端输出（可能是错误信息、日志、命令结果）。

请说明：
1. 这段输出在说什么；
2. 如果是错误或异常，最可能的原因是什么；
3. 下一步可以做什么（给出具体命令时用行内代码标注）。

可以用 Markdown（列表、行内代码、代码块）。用中文回答。
如果这段输出里看不出问题，直接说看不出问题——不要为了凑内容而编造原因。";

/// 一次 NL→命令的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSuggestion {
    /// 建议的命令（已剥围栏、已 trim）。
    pub command: String,
    /// 模型给的解释。
    pub explanation: String,
    /// **我们自己算的**策略分级：`read_only` / `write` / `dangerous`。
    pub tier: String,
    /// 命中的规则理由（人话），逐条给 UI。
    pub reasons: Vec<String>,
    /// 当前档位下的裁决：`auto` / `confirm` / `deny`。
    pub decision: String,
    /// 这次请求有没有把屏幕内容发出去。用户有权知道。
    pub included_screen: bool,
    /// 脱敏命中（规则名 → 次数）。空表示这条 prompt 本来就干净。
    pub redactions: Vec<(String, usize)>,
}

/// 从模型回答里抠出 COMMAND 与 WHY 两段。
///
/// 宽容解析：模型偶尔会多写一行寒暄、把标签写成小写、或者仍然套上代码围栏。
/// 这些都不该让整次请求失败——用户看到的会是「AI 出错了」，而实际上答案就在那儿。
///
/// 但**不宽容到猜**：一行 COMMAND 都找不到时返回 `None`，由调用方报「没读懂模型的回答」
/// 并把原文给用户看。凭空把整段回答当成命令是最坏的处置——那可能是一段道歉。
pub fn parse_suggestion(reply: &str) -> Option<(String, String)> {
    let mut command: Option<String> = None;
    let mut why_lines: Vec<String> = Vec::new();
    let mut in_why = false;

    for line in reply.lines() {
        let t = line.trim();
        let lower = t.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("command:") {
            // 用原行切，避免把大小写混合的内容压成小写
            let raw = t[t.len() - rest.len()..].trim();
            command = Some(strip_code_fence(raw).to_string());
            in_why = false;
            continue;
        }
        if let Some(rest) = lower.strip_prefix("why:") {
            let raw = t[t.len() - rest.len()..].trim();
            if !raw.is_empty() {
                why_lines.push(raw.to_string());
            }
            in_why = true;
            continue;
        }
        // WHY 之后的续行都算解释（模型经常换行写）
        if in_why && !t.is_empty() && !t.starts_with("```") {
            why_lines.push(t.to_string());
        }
    }

    let cmd = command?;
    if cmd.is_empty() {
        return None;
    }
    Some((cmd, why_lines.join("\n")))
}

/// 剥掉单行里的代码围栏残留。
///
/// 模型很爱写 ```` ```bash ls -la``` ````。这里只处理**同一行内**的围栏——
/// 跨行的围栏由上面的逐行解析天然吃掉（围栏行不含 `COMMAND:`，不会被当成命令）。
fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix("```").unwrap_or(s);
    let s = s.strip_suffix("```").unwrap_or(s);
    // ```bash ls -la  →  剥掉语言标记
    let s = s.trim();
    for lang in ["bash ", "sh ", "shell ", "zsh ", "console "] {
        if let Some(rest) = s.strip_prefix(lang) {
            return rest.trim();
        }
    }
    s
}

/* ─────────────────────── 调用模型（M2 出口第 8、12、13、14 项）─────────────────────── */

/// 一次请求的通用前置：取活动配置 + 组装上下文。
///
/// 返回 `Err` 的两种情形都是**发请求之前**就该拦住的：没配 provider、总开关关着。
/// 「关着但点一下就能跑」不是开关，是提示——所以档位检查在这里，不在发送之后。
/// pub(crate)：Agent 的启动期装配复用同一份（provider 选取 + 档位 + 上下文）。
/// 两份实现就是两套「没配置怎么办」的答案。
pub(crate) async fn prepare(
    state: &AppState,
    screen: Option<String>,
    session_id: Option<&str>,
) -> Result<
    (
        ProviderConfig,
        fs_ai::context::AssembledContext,
        fs_policy::AiMode,
    ),
    String,
> {
    let entries = load_entries(state).await;
    let mut configs = Vec::new();
    for e in &entries {
        if let Some(c) = to_config(state, e).await {
            configs.push(c);
        }
    }
    let cfg = match ProviderState::pick(&configs) {
        ProviderState::Unconfigured => {
            return Err("还没配置任何模型源。在 AI 面板的「配置」页填一个。".into())
        }
        ProviderState::Configured(c) => c,
    };

    let mode = read_mode(state).await;
    if mode == fs_policy::AiMode::Disabled {
        return Err("AI 执行总开关是关着的（设置 → AI）。".into());
    }

    // Vault 里的密钥用于脱敏。取不到（锁着）时给空表——**脱敏仍然生效**，
    // 只是少了「已知密钥确切替换」那一路，正则那几路照旧。
    // 不因为拿不到密钥表就跳过脱敏：那会让锁着 Vault 的用户反而发出更多东西。
    let known = known_secrets(state).await;

    // host facts 与 profile 元信息都按 session 取。取不到时给空值而**不是**「未知」——
    // `fs_ai::context` 那边的约定是「缺项不写占位符」，因为写了会让模型把它当事实推理
    // （「shell 是 unknown」会引出一串关于 unknown shell 的推测）。
    let (host_facts, profile) = match session_id {
        None => (
            fs_ai::context::HostFacts::default(),
            fs_ai::context::ProfileFacts::default(),
        ),
        Some(sid) => {
            let facts = state
                .host_facts
                .lock()
                .await
                .get(sid)
                .cloned()
                .unwrap_or_default();
            (facts, profile_facts_of(state, sid).await)
        }
    };

    let inputs = fs_ai::context::ContextInputs {
        profile,
        host_facts,
        screen: screen
            .as_deref()
            .map(fs_ai::context::ScreenExcerpt::from_grid_text),
        allow_screen_context: cfg.allow_screen_context,
        known_secrets: known,
    };
    Ok((cfg, fs_ai::context::assemble(&inputs), mode))
}

/// 取会话对应的 profile 元信息（连接名/主机/端口/用户）。
///
/// 从**注册表里那条会话记着的 profile_id** 反查，而不是让前端把这些字段传上来：
/// 前端传的话，一个被改过的请求就能让上下文里写着「你正连着 prod-db」
/// 而实际连的是别处——而模型会照着那句话推理。
async fn profile_facts_of(state: &AppState, session_id: &str) -> fs_ai::context::ProfileFacts {
    let Some(session) = state.registry.get(session_id) else {
        return fs_ai::context::ProfileFacts::default();
    };
    let profiles = match fs_connmgr::ProfileRepo::new(state.db.pool()).list().await {
        Ok(p) => p,
        Err(_) => return fs_ai::context::ProfileFacts::default(),
    };
    match profiles
        .iter()
        .find(|p| p.id.to_string() == session.profile_id)
    {
        None => fs_ai::context::ProfileFacts::default(),
        Some(p) => fs_ai::context::ProfileFacts {
            name: Some(p.name.clone()),
            host: Some(p.host.clone()),
            port: Some(p.port),
            user: Some(p.username.clone()),
        },
    }
}

/// 读档位。认不出的串一律回落 `Disabled`。
///
/// **回落最严的那一档**，不是最宽的：一个被写脏的档位键不该变成「随便跑」。
/// 这与 `parse_kind` 的「认不出即 None」是同一条原则的两面——
/// 那边是「不猜协议」，这边是「不猜权限」。
pub(crate) async fn read_mode(state: &AppState) -> fs_policy::AiMode {
    let raw = fs_connmgr::SettingsRepo::new(state.db.pool())
        .get(AI_MODE_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<String>(&s).ok());
    match raw.as_deref() {
        // 键不存在 = 默认档（理由见 ai_status 里那段注释）
        None => fs_policy::AiMode::WithConfirm,
        // 串 → 枚举用 fs_policy 自己的逆函数，不在这里抄一份 match。
        // 起草时抄了一份，把 ReadOnlyOnly 写成了 ReadOnly、把稳定串写成了
        // 连字符版（真值是下划线版 read_only）——编译器拦下了前者，
        // 而后者会静默地让「只读档」这个选项永远选不上。
        Some(other) => fs_policy::AiMode::from_str_exact(other)
            // 认不出的串回落**最严**的那一档，不是最宽的：一个被写脏的档位键
            // 不该变成「随便跑」。这与 parse_kind 的「认不出即 None」是
            // 同一条原则的两面——那边是不猜协议，这边是不猜权限。
            .unwrap_or(fs_policy::AiMode::Disabled),
    }
}

/// Vault 里所有 secret 的明文，用于脱敏时的「确切替换」。
///
/// 这是唯一有强保证的一路：正则能认出「像密钥的东西」，但认不出用户那个
/// 恰好长得像普通单词的口令。取全量而不只取当前 provider 的 key——
/// 屏幕上出现的可能是任何一条（他刚才用 echo 打出来的那个数据库密码）。
pub(crate) async fn known_secrets(state: &AppState) -> Vec<String> {
    let guard = state.vault.lock().await;
    let Some(store) = guard.as_ref() else {
        return Vec::new();
    };
    store
        .list()
        .into_iter()
        .filter_map(|m| store.get(m.id).ok())
        .filter_map(|b| String::from_utf8(b.to_vec()).ok())
        // 太短的串不进脱敏表：一个两字符的密钥会把正文涂成筛子。
        // 8 是个折中——比它短的口令本来就不该存在，而误涂的代价是让解读结果不可读。
        .filter(|s| s.trim().chars().count() >= 8)
        .collect()
}

/// NL→命令（M2 出口第 8 项）。
#[tauri::command]
pub async fn ai_suggest_command(
    prompt: String,
    screen: Option<String>,
    session_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandSuggestion, String> {
    if prompt.trim().is_empty() {
        return Err("说一下你想做什么".into());
    }
    let (cfg, ctx, mode) = prepare(&state, screen, session_id.as_deref()).await?;

    let req = ChatRequest {
        system: NL_TO_CMD_SYSTEM.to_string(),
        user: format!("{}\n\n用户的意图：{}", ctx.text, prompt.trim()),
        max_tokens: 1024,
        stream: false,
    };
    let reply = call_model(&cfg, &req).await?;

    let (command, explanation) = parse_suggestion(&reply).ok_or_else(|| {
        // 不猜：把原文给用户看。凭空把整段回答当成命令是最坏的处置——
        // 那可能是一段道歉，而它会被填进输入框等着回车。
        format!("没读懂模型的回答（下面是原文）：\n{reply}")
    })?;

    // **策略预判在这里现算**，不问模型（见模块头）。
    let verdict = fs_policy::classify(&command);
    let decision = fs_policy::gate::decide(verdict.tier, mode);

    // 审计：AI 建议了什么、判成什么级。**建议本身就要落库**——
    // 只记「执行了什么」的话，一次被拒的危险建议不留痕，而那正是最该留痕的。
    audit_ai(
        &state,
        &command,
        session_id.as_deref(),
        verdict.tier,
        // Confirm/StrongConfirm 这两档此刻**还没执行**——用户还没点。
        //
        // 这里曾记 `Approved`，注释写着「不准确但枚举里没有『待确认』档」。
        // 那句话是错的：`RequestOnly` 的语义正是「只是一次请求，没有要执行的
        // 东西」，Agent 侧的双行制早就用它表达「发起了、未执行」（dispatch.rs
        // 的第 1 行）。记 Approved 的实际后果是——**一条被用户拒绝的危险建议
        // 在审计里永远显示为已批准**，且 `Verdict::executed()` 对它返回真。
        // 链在密码学上完整、在语义上是错的，而审计的全部价值就在语义。
        //
        // 真正的执行结果由执行路径另记一条（Approved/Rejected），两条按 action
        // 与时间对得上：一次「建议 → 被拒」在链上是 RequestOnly + Rejected 两行，
        // 而不是一行谎称已批准。
        match decision {
            fs_policy::gate::Decision::AutoRun => fs_connmgr::audit_repo::Verdict::AutoRun,
            fs_policy::gate::Decision::Confirm | fs_policy::gate::Decision::StrongConfirm => {
                fs_connmgr::audit_repo::Verdict::RequestOnly
            }
            fs_policy::gate::Decision::Deny(_) => fs_connmgr::audit_repo::Verdict::Denied,
        },
    )
    .await;

    Ok(CommandSuggestion {
        command,
        explanation,
        tier: verdict.tier.as_str().to_string(),
        reasons: verdict.reasons.iter().map(|r| r.detail.clone()).collect(),
        // 四档各有自己的串。**StrongConfirm 不能折成 confirm**：两者的区别全在
        // 手势成本上（单次确认能被肌肉记忆点掉，强确认不能），折了就等于
        // 让 write 与 dangerous 在界面上没有区别——而那是分级的全部意义。
        decision: match decision {
            fs_policy::gate::Decision::AutoRun => "auto",
            fs_policy::gate::Decision::Confirm => "confirm",
            fs_policy::gate::Decision::StrongConfirm => "strong-confirm",
            fs_policy::gate::Decision::Deny(_) => "deny",
        }
        .to_string(),
        included_screen: ctx.included_screen,
        redactions: ctx
            .redaction
            .hits
            .iter()
            .map(|(r, n)| (r.to_string(), *n))
            .collect(),
    })
}

/// 一次解读的结果（M2 出口第 12 项）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Explanation {
    /// Markdown 原文。**前端负责净化渲染**——这里不做 HTML 转换，
    /// 因为转换出的 HTML 一旦经 IPC 传过去，前端就只能选择信任它。
    /// 让前端拿到 Markdown、自己用净化器渲染，信任边界才在正确的位置。
    pub markdown: String,
    pub included_screen: bool,
    pub redactions: Vec<(String, usize)>,
}

#[tauri::command]
pub async fn ai_explain(
    selection: String,
    session_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Explanation, String> {
    if selection.trim().is_empty() {
        return Err("先在终端里选中一段输出".into());
    }
    // 选中的那段**就是**要解读的内容，所以它走 screen 这一路——
    // 于是「允许发送屏幕上下文」这个开关同样管着它。绕过那个开关把选区
    // 单独发出去，等于让开关名不副实。
    let (cfg, ctx, _mode) = prepare(&state, Some(selection.clone()), session_id.as_deref()).await?;
    if !ctx.included_screen {
        return Err("这个模型源不允许发送屏幕内容（AI 面板 → 配置 → 允许发送屏幕上下文）。解读功能需要把你选中的那段输出发出去。".into());
    }

    let req = ChatRequest {
        system: EXPLAIN_SYSTEM.to_string(),
        user: ctx.text.clone(),
        max_tokens: 2048,
        stream: false,
    };
    let markdown = call_model(&cfg, &req).await?;

    audit_ai(
        &state,
        "AI 解读选中输出",
        session_id.as_deref(),
        fs_policy::Tier::ReadOnly,
        fs_connmgr::audit_repo::Verdict::AutoRun,
    )
    .await;

    Ok(Explanation {
        markdown,
        included_screen: ctx.included_screen,
        redactions: ctx
            .redaction
            .hits
            .iter()
            .map(|(r, n)| (r.to_string(), *n))
            .collect(),
    })
}

/// 发一次请求并取出正文。
async fn call_model(cfg: &ProviderConfig, req: &ChatRequest) -> Result<String, String> {
    let spec = wire::build_request(cfg, req).map_err(|e| e.user_message())?;
    let transport = fs_ai::transport::ReqwestTransport::new().map_err(|e| e.to_string())?;
    let resp = fs_ai::transport::Transport::send(&transport, &spec)
        .await
        .map_err(|e| e.user_message())?;
    if !resp.is_success() {
        return Err(wire::map_error(resp.status, &resp.body, resp.retry_after_secs).user_message());
    }
    let parsed = wire::parse_response(cfg.kind, &resp.body).map_err(|e| e.user_message())?;
    if parsed.text.trim().is_empty() {
        // 空回答不是成功。静默返回空串会让界面显示一个空白气泡，
        // 而用户会以为是自己的问题没说清。
        return Err("模型返回了空回答（可能触发了内容过滤，或 max_tokens 太小）".into());
    }
    Ok(parsed.text)
}

/// 把一次 AI 动作落进审计表（M2 出口第 13 项）。
///
/// # 失败只警告，不让整次请求失败
///
/// 审计写不进去（库锁着、磁盘满）时，用户已经拿到了模型的回答——这时报错
/// 等于把一次成功的请求说成失败，而他会重试，于是又发一次请求、又花一次钱。
/// 审计的价值在于事后可查，而「这一条没记上」比「用户以为功能坏了」轻。
///
/// 但它必须**留痕在日志里**：静默丢弃会让「审计表为什么少了一条」变成无解之谜。
async fn audit_ai(
    state: &AppState,
    action: &str,
    session: Option<&str>,
    tier: fs_policy::Tier,
    verdict: fs_connmgr::audit_repo::Verdict,
) {
    // 落库前再脱敏一遍。上游已经脱过 prompt，但**命令本身**是模型新造的文本，
    // 它可能把屏幕上看到的密钥抄进了命令里（`mysql -p<密码>` 就是这么来的）。
    let known = known_secrets(state).await;
    let refs: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
    let redacted = fs_ai::redact::redact(action, &refs);

    let entry = fs_connmgr::audit_repo::NewAuditEntry {
        actor: fs_connmgr::audit_repo::Actor::Ai,
        action: redacted.text,
        target_session: session.map(|s| s.to_string()),
        risk_level: tier.as_str().to_string(),
        verdict,
        exit_code: None,
        output_digest: None,
        created_at: now_rfc3339(),
    };
    if let Err(e) = fs_connmgr::audit_repo::AuditRepo::new(state.db.pool())
        .append(&entry)
        .await
    {
        tracing::warn!(error = %e, "AI 动作审计写入失败（请求本身已成功）");
    }
}

/// 当前时间的 RFC3339 串。
///
/// `audit_repo` 刻意不自己取时间（那会让它不可测），所以时间在这一层取。
pub(crate) fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // 不引 chrono：整个仓库只需要这一处「现在几点的 RFC3339 串」，
    // 为它加一个依赖不划算。手算的代价是这几行，而它有测试。
    format_rfc3339(now.as_secs())
}

/// Unix 秒 → `YYYY-MM-DDTHH:MM:SSZ`。
/// pub(crate)：audit_cmd 的取证包导出时间戳也用这一份。日期数学是最不该复制的东西——
/// 本文件这条曾经算错过一次（%86400 的余数看错成 02:26:40），复制两份就是两处可错。
pub(crate) fn format_rfc3339(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// 天数（自 1970-01-01）→ 年月日。Howard Hinnant 的 civil_from_days。
///
/// 抄一个公开算法而不是手推：闰年与世纪闰年的边界是这类代码最常错的地方，
/// 而错法是「某一天的日期差一天」——那种错在审计表里意味着时间线对不上。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// 配置校验错误的人话说明**不在这里**：`fs_ai::provider::ConfigError::message()` 已经有了，
// 而且写得更准（「本程序不内置任何默认地址」这句解释了为什么地址是必填的）。
// 起草时这里抄了一份，四个变体里还抄错一个（凭空多了个 ApiKeyBlank）——
// 编译器当场拦下。两份文案的问题不止是重复：它们会分叉，而分叉之后
// 用户在两个地方看到对同一件事的两种说法。

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> ProviderEntry {
        ProviderEntry {
            id: "p1".into(),
            kind: "anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            model: "claude-sonnet-5".into(),
            key_ref: Some(7),
            allow_screen_context: true,
        }
    }

    #[test]
    fn kind_strings_round_trip() {
        for (s, k) in [
            ("anthropic", ProviderKind::Anthropic),
            ("openai", ProviderKind::OpenAiCompatible),
            ("ollama", ProviderKind::Ollama),
        ] {
            assert_eq!(parse_kind(s), Some(k));
            assert_eq!(kind_str(k), s);
        }
    }

    /// 认不出的 kind 必须是 `None`，**不许回落**。
    ///
    /// 回落的后果很具体：一个拼错的 kind 会被当成另一家的协议，于是请求体是
    /// Anthropic 的形状、发到 OpenAI 的地址——用户看到一句 400，
    /// 而真正的问题是那个串拼错了。
    #[test]
    fn an_unknown_kind_is_none_not_a_default() {
        for bad in ["Anthropic", "claude", "", "gpt", "openai-compatible"] {
            assert_eq!(parse_kind(bad), None, "{bad}");
        }
    }

    /// **给前端的状态里不许有 key，也不许有 key_ref。**
    ///
    /// 连记录 id 都不给：前端拿它没用处（保存时由用户重新输入 key），
    /// 而一个记录 id 加上 `vault_get_secret` 就是一条取 key 的路径。
    #[test]
    fn the_status_payload_carries_no_key_and_no_key_ref() {
        let s = AiStatus {
            configured: true,
            active: Some(ActiveProvider {
                kind: "anthropic".into(),
                base_url: "https://api.anthropic.com".into(),
                model: "claude-sonnet-5".into(),
                allow_screen_context: true,
            }),
            entries: vec![ProviderSummary {
                id: "p1".into(),
                kind: "anthropic".into(),
                base_url: "https://api.anthropic.com".into(),
                model: "claude-sonnet-5".into(),
                has_key: true,
                allow_screen_context: true,
                problem: None,
            }],
            mode: "with-confirm".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("key_ref"), "{json}");
        assert!(!json.contains("api_key"), "{json}");
        // `has_key` 是布尔，不是值
        assert!(json.contains("\"has_key\":true"));
        // 反向对照：确实有内容（否则空载荷让上面几条恒真）
        assert!(json.contains("claude-sonnet-5"));
    }

    /// 摘要结构里没有任何能装下 key 的字段。
    ///
    /// 比「序列化时过滤」硬：加字段时这条会红，逼着人回答「那个字段会不会带出 key」。
    #[test]
    fn the_summary_struct_has_no_secret_shaped_field() {
        const SRC: &str = include_str!("ai_cmd.rs");
        let head = SRC.find("pub struct ProviderSummary {").expect("找不到");
        let start = head + SRC[head..].find('{').expect("没有左花括号") + 1;
        let body = &SRC[start..start + SRC[start..].find("\n}").expect("没闭合")];
        let fields: Vec<&str> = body
            .lines()
            .filter(|l| l.trim_start().starts_with("pub "))
            .collect();
        assert_eq!(fields.len(), 7, "ProviderSummary 字段变了：{fields:?}");
        for f in &fields {
            let lower = f.to_ascii_lowercase();
            // 判据不是「名字里不许有 key」——`has_key: bool` 是**正确**的字段，
            // 它只说「有没有」。首跑时判据写成「名字不许含 key」，当场把它误判成违例。
            //
            // 真正的判据是：名字里带 key/secret/token 的字段，**类型必须是 bool**。
            // 于是 `has_key: bool` 合法，而将来有人加一个 `api_key: Option<String>`
            // 或 `token: String` 会立刻红。
            let secretish =
                lower.contains("key") || lower.contains("secret") || lower.contains("token");
            if secretish {
                assert!(
                    f.contains(": bool"),
                    "名字像密钥的字段只能是 bool（只说有没有），不能装值：{f}"
                );
            }
            // 顺带钉住整体：所有字段都只装 String / Option<String> / bool——
            // 都装不下一份 key 材料以外的东西，也装不下 key_ref 那种可以换取 key 的句柄。
            assert!(
                f.contains("String") || f.contains("bool"),
                "出现了既非 String 也非 bool 的字段：{f}"
            );
        }
    }

    /// 每条校验错误都点名是**哪一项**没填对。
    ///
    /// 「配置无效」这种话让用户唯一的下一步是把三个框全清了重填。
    /// 文案本体在 `fs_ai`（本文件不再抄一份），这条守的是那份文案的可操作性——
    /// 它是本文件唯一的消费方，而 `fs_ai` 那边没有这条断言。
    #[test]
    fn every_config_error_names_which_field_is_wrong() {
        use fs_ai::provider::ConfigError as C;
        for e in [
            C::BaseUrlEmpty,
            C::BaseUrlNotHttp,
            C::ModelEmpty,
            C::ApiKeyMissing,
        ] {
            let t = e.message();
            assert!(t.chars().count() >= 5, "{e:?} → {t}");
            assert!(
                t.contains("地址") || t.contains("模型") || t.contains("key"),
                "{e:?} → {t} 没点名是哪一项"
            );
        }
    }

    #[test]
    fn a_half_filled_entry_is_reported_as_a_problem_not_silently_dropped() {
        // 出口原文：「不崩溃、**不降级为假数据**」。一份填了一半的配置必须显示成
        // 「这条还没填完」，而不是安静地不生效——后者会让用户以为配好了。
        let mut e = entry();
        e.model = String::new();
        let cfg = ProviderConfig {
            kind: ProviderKind::Anthropic,
            base_url: e.base_url.clone(),
            model: e.model.clone(),
            api_key: Some("k".into()),
            allow_screen_context: false,
        };
        let problem = cfg.validate().err().map(|x| x.message().to_string());
        assert!(problem.is_some());
        assert!(problem.unwrap().contains("模型"));
    }

    #[test]
    fn serde_defaults_fail_closed_on_screen_context() {
        // 忘写这个字段时的行为必须是「不发屏幕内容」。
        let e: ProviderEntry =
            serde_json::from_str(r#"{"id":"p","kind":"ollama","base_url":"http://x","model":"m"}"#)
                .unwrap();
        assert!(!e.allow_screen_context);
        assert_eq!(e.key_ref, None);
    }

    /* ── 提示模板与回答解析（M2 出口第 8 项）────────────────────────────── */

    #[test]
    fn a_well_formed_reply_parses() {
        let (cmd, why) = parse_suggestion("COMMAND: ls -la /var/log\nWHY: 列出日志目录的全部文件")
            .expect("标准格式应当解析得出");
        assert_eq!(cmd, "ls -la /var/log");
        assert_eq!(why, "列出日志目录的全部文件");
    }

    /// 模型很爱多写一行寒暄、把标签写成小写、或者仍然套上代码围栏。
    /// 这些都不该让整次请求失败——用户看到的会是「AI 出错了」，而答案就在那儿。
    #[test]
    fn common_model_sloppiness_is_tolerated() {
        for reply in [
            "好的，这是命令：\nCOMMAND: df -h\nWHY: 看磁盘占用",
            "command: df -h\nwhy: 看磁盘占用",
            "COMMAND: ```bash df -h```\nWHY: 看磁盘占用",
            "COMMAND:    df -h   \nWHY:   看磁盘占用  ",
        ] {
            let (cmd, why) = parse_suggestion(reply).unwrap_or_else(|| panic!("{reply:?}"));
            assert_eq!(cmd, "df -h", "{reply:?}");
            assert!(why.contains("磁盘"), "{reply:?}");
        }
    }

    #[test]
    fn a_multi_line_why_is_kept_whole() {
        // 模型经常把解释换行写。只取第一行会把「所以要加 -h」这种关键补充丢掉。
        let (_, why) =
            parse_suggestion("COMMAND: df -h\nWHY: 看磁盘占用。\n-h 让输出用人类可读的单位。")
                .unwrap();
        assert!(why.contains("人类可读"), "{why}");
    }

    /// **读不懂就说读不懂，不猜。**
    ///
    /// 把整段回答当成命令是最坏的处置——那可能是一段道歉，而它会被填进输入框
    /// 等着用户按回车。
    #[test]
    fn an_unparseable_reply_is_none_not_a_guess() {
        for reply in [
            "抱歉，我不能帮你做这件事。",
            "",
            "WHY: 只有解释没有命令",
            "COMMAND:",
            "COMMAND:   ",
        ] {
            assert_eq!(parse_suggestion(reply), None, "{reply:?}");
        }
    }

    #[test]
    fn code_fences_are_stripped_including_language_tags() {
        for (raw, want) in [
            ("```bash ls```", "ls"),
            ("```sh ls -la```", "ls -la"),
            ("```ls```", "ls"),
            ("ls", "ls"),
            ("```console echo hi```", "echo hi"),
        ] {
            assert_eq!(strip_code_fence(raw), want, "{raw:?}");
        }
    }

    /// 提示模板里必须明确禁止模型自评危险度。
    ///
    /// 这不是文案偏好：模型一旦开始输出「这条命令是安全的」，界面上就会同时存在
    /// 两个关于危险度的说法——一个来自 `fs_policy`，一个来自可以被提示注入影响的模型。
    /// 用户不知道该信哪个，而那比只有一个判据糟得多。
    #[test]
    fn the_prompt_forbids_the_model_from_judging_danger() {
        assert!(
            NL_TO_CMD_SYSTEM.contains("不要评估这条命令的危险程度"),
            "提示里不再禁止模型自评危险度"
        );
        assert!(NL_TO_CMD_SYSTEM.contains("程序自己判定"));
    }

    #[test]
    fn the_prompt_forbids_markdown_fences_in_the_command() {
        // 要求纯文本比在解析侧剥围栏可靠——剥围栏要处理各种变体，漏一种就是一次失败执行。
        // 两道都留着（提示 + strip_code_fence），因为模型不一定听话。
        assert!(NL_TO_CMD_SYSTEM.contains("代码围栏"));
    }

    /// 解读的提示**允许** Markdown，与 NL→命令那条相反。
    ///
    /// 差别在产物的去处：命令要进输入框执行，Markdown 是污染；
    /// 解读只给人读，Markdown 是可读性。
    #[test]
    fn the_explain_prompt_allows_markdown_but_forbids_making_things_up() {
        assert!(EXPLAIN_SYSTEM.contains("Markdown"));
        assert!(
            EXPLAIN_SYSTEM.contains("不要为了凑内容而编造"),
            "解读提示里不再禁止编造原因"
        );
    }

    /* ── 策略预判是我们算的（模块头那条不变量）─────────────────────────── */

    /// 建议里的 `tier` 与 `reasons` 必须来自 `fs_policy::classify`，不是模型说的。
    ///
    /// 用一条模型「声称安全」的危险命令来验：如果哪天有人改成信模型的自评，
    /// 这一条会立刻红。
    #[test]
    fn the_tier_comes_from_policy_not_from_the_model() {
        let reply = "COMMAND: rm -rf /\nWHY: 这条命令完全安全，只是清理一些临时文件。";
        let (cmd, why) = parse_suggestion(reply).unwrap();
        // 模型的说法原样保留在 explanation 里（用户有权看到它说了什么）
        assert!(why.contains("完全安全"));
        // 但危险度是我们算的
        let verdict = fs_policy::classify(&cmd);
        assert_eq!(verdict.tier, fs_policy::Tier::Dangerous);
        assert!(!verdict.reasons.is_empty(), "危险判定必须给出理由");
    }

    /// 源码断言：本文件不得把模型回答里的任何字段当成危险度判据。
    ///
    /// 行为测试证明的是当下这一版对；这一条防的是**将来**有人加一行
    /// 「如果模型说 SAFE 就跳过确认」——那种改动看起来像优化（少弹一个框），
    /// 实际是把审查权交给被审查者。
    #[test]
    fn no_code_path_reads_a_danger_verdict_out_of_the_model_reply() {
        const SRC: &str = include_str!("ai_cmd.rs");
        let prod = &SRC[..SRC.find("\n#[cfg(test)]").expect("本文件必须有测试段")];
        for banned in ["SAFE", "DANGER:", "RISK:", "model_tier", "self_assessed"] {
            assert!(
                !prod.contains(banned),
                "生产代码里出现了 {banned}——危险度只能由 fs_policy::classify 算"
            );
        }
        // 反向对照：classify 确实被调用了（否则上面几条会因为整段逻辑都没了而假绿）
        assert!(prod.contains("fs_policy::classify(&command)"));
    }

    /// 四档裁决各有自己的串，`StrongConfirm` 不许折成 `confirm`。
    ///
    /// 两者的区别全在手势成本上（单次确认能被肌肉记忆点掉，强确认不能）。
    /// 折了就等于让 write 与 dangerous 在界面上没有区别——而那是分级的全部意义。
    #[test]
    fn strong_confirm_is_not_collapsed_into_confirm() {
        const SRC: &str = include_str!("ai_cmd.rs");
        let prod = &SRC[..SRC.find("\n#[cfg(test)]").expect("需要测试段")];
        assert!(
            prod.contains("\"strong-confirm\""),
            "StrongConfirm 没有独立的串"
        );
        // 且四档都在
        for s in ["\"auto\"", "\"confirm\"", "\"strong-confirm\"", "\"deny\""] {
            assert!(prod.contains(s), "缺少裁决串 {s}");
        }
    }

    /* ── 档位（M2 出口第 14 项）──────────────────────────────────────── */

    /// 认不出的档位串回落**最严**的那一档。
    ///
    /// 一个被写脏的档位键不该变成「随便跑」。这与 `parse_kind` 的「认不出即 None」
    /// 是同一条原则的两面：那边不猜协议，这边不猜权限。
    #[test]
    fn an_unknown_mode_string_falls_back_to_disabled() {
        for bad in ["", "yolo", "READ_ONLY", "read-only", "with-confirm"] {
            // 注意 read-only / with-confirm 这两个**连字符版**也在这里：
            // fs_policy 的稳定串是下划线版（read_only / with_confirm）。
            // 起草时这里抄错成连字符版，那会让「只读档」永远选不上——
            // 而症状是「选了只读，写命令照样弹确认框」。
            assert_eq!(
                fs_policy::AiMode::from_str_exact(bad),
                None,
                "{bad} 不该被认成合法档位"
            );
        }
        // 反向对照：真值认得出
        for (s, m) in [
            ("disabled", fs_policy::AiMode::Disabled),
            ("read_only", fs_policy::AiMode::ReadOnlyOnly),
            ("with_confirm", fs_policy::AiMode::WithConfirm),
        ] {
            assert_eq!(fs_policy::AiMode::from_str_exact(s), Some(m));
        }
    }

    /// 总开关关着时，**连确认框都不弹**。
    #[test]
    fn the_disabled_mode_denies_even_read_only() {
        for tier in [
            fs_policy::Tier::ReadOnly,
            fs_policy::Tier::Write,
            fs_policy::Tier::Dangerous,
        ] {
            let d = fs_policy::gate::decide(tier, fs_policy::AiMode::Disabled);
            assert!(
                matches!(d, fs_policy::gate::Decision::Deny(_)),
                "{tier:?} 在 Disabled 档下应当是 Deny，实得 {d:?}"
            );
        }
    }

    /// **总开关的执法点必须在 `prepare` 里**（M2 出口第 14 项）。
    ///
    /// 上一条测的是 `fs_policy::decide` 的档位表——那张表一直是对的。但它测不到
    /// 一件更要紧的事：**ai_cmd 有没有真的去执法**。交叉审计用变异实证了这个洞：
    /// 删掉 `prepare` 里那三行 Disabled 早返回，313 条测试全绿。
    ///
    /// `prepare` 要 `AppState` + 真库，行为测试跑不动；而这条不变量说的是
    /// 「那个早返回在不在」，源码扫描读得出来就够（同仓多处先例）。
    ///
    /// 为什么执法点必须在 `prepare` 而不是各命令自己判：`ai_suggest_command` 与
    /// `ai_explain` 都经它，收在一处才不会有人新加一个 AI 命令时忘了判。
    #[test]
    fn the_global_switch_is_enforced_in_prepare_not_only_in_the_policy_table() {
        let src = include_str!("ai_cmd.rs");
        let prod = match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        };
        // 取 prepare 的函数体（到下一个顶层 `pub` 或 `async fn` 为止够用）
        let i = prod
            .find("pub(crate) async fn prepare(")
            .expect("prepare 必须在——扫查坏了");
        let body = &prod[i..(i + 3000).min(prod.len())];
        assert!(
            body.contains("fs_policy::AiMode::Disabled"),
            "prepare 里必须有总开关判定：删掉它，NL→命令与解读两路都会在关闭档下照常发请求"
        );
        assert!(
            body.contains("return Err("),
            "总开关判定必须是**早返回**，不是记个日志继续走"
        );
        // 做空防护：确认扫到的是真函数体（它必然读档位）
        assert!(
            body.contains("read_mode(state)"),
            "扫查坏了：没取到 prepare 的体"
        );
    }

    /* ── 时间格式化（审计行用）──────────────────────────────────────── */

    /// `audit_repo` 刻意不自己取时间（那会让它不可测），所以时间在这一层取。
    /// 而手算 RFC3339 最容易错在闰年与世纪闰年的边界上，错法是「差一天」——
    /// 那种错在审计表里意味着时间线对不上。
    #[test]
    fn rfc3339_formatting_is_correct_at_the_hard_boundaries() {
        for (secs, want) in [
            (0u64, "1970-01-01T00:00:00Z"),
            (86_399, "1970-01-01T23:59:59Z"),
            (86_400, "1970-01-02T00:00:00Z"),
            // 1972 是闰年：2 月 29 日存在
            (68_169_600, "1972-02-29T00:00:00Z"),
            // 2000 是世纪闰年（能被 400 整除）
            (951_782_400, "2000-02-29T00:00:00Z"),
            // 2100 不是闰年（能被 100 整除但不能被 400 整除）
            (4_107_542_400, "2100-03-01T00:00:00Z"),
            // 1756000000 % 86400 = 6400 秒 = 01:46:40（首写时把这个余数算错了，
            // 实现反而是对的——这类手算期望值本身就是个容易出错的地方）
            (1_756_000_000, "2025-08-24T01:46:40Z"),
        ] {
            assert_eq!(format_rfc3339(secs), want, "secs={secs}");
        }
    }

    #[test]
    fn now_is_a_plausible_rfc3339_string() {
        let s = now_rfc3339();
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.ends_with('Z'), "{s}");
        // 年份落在一个合理区间——差一个数量级的话上面那些边界用例可能碰巧都过了
        let year: i64 = s[..4].parse().expect("前四位应当是年份");
        assert!((2024..2100).contains(&year), "{s}");
    }

    /* ── 屏幕上下文开关（M2 出口第 14 项后半）───────────────────────── */

    /// 解读功能走 `screen` 那一路，于是「允许发送屏幕上下文」同样管着它。
    ///
    /// 绕过那个开关把选区单独发出去，等于让开关名不副实——用户关掉它的意思是
    /// 「别把我屏幕上的东西发出去」，而选区就是屏幕上的东西。
    #[test]
    fn explain_respects_the_screen_context_switch() {
        const SRC: &str = include_str!("ai_cmd.rs");
        let prod = &SRC[..SRC.find("\n#[cfg(test)]").expect("需要测试段")];
        let start = prod
            .find("pub async fn ai_explain(")
            .expect("找不到 ai_explain");
        let body = &prod[start..];
        let end = body.find("\n}\n").unwrap_or(body.len());
        let body = &body[..end];
        // 选区走 prepare 的 screen 参数（而不是另开一条绕过开关的路）
        assert!(
            body.contains("prepare(&state, Some(selection.clone())"),
            "ai_explain 不再把选区当作 screen 上下文——那会绕过按 provider 的开关"
        );
        // 且开关关着时明确拒绝，不静默发一个没有选区的请求
        assert!(body.contains("if !ctx.included_screen"));
    }

    /// 组装上下文时，屏幕开关取自**该 provider** 的配置，不是某个全局值。
    #[test]
    fn the_screen_switch_is_per_provider() {
        const SRC: &str = include_str!("ai_cmd.rs");
        let prod = &SRC[..SRC.find("\n#[cfg(test)]").expect("需要测试段")];
        assert!(
            prod.contains("allow_screen_context: cfg.allow_screen_context"),
            "上下文组装不再读该 provider 自己的屏幕开关"
        );
    }

    /// **待确认的建议不得记成已执行**（M3 出口 7 的 ②）。
    ///
    /// 这里曾把 Confirm/StrongConfirm 记成 `Approved`，于是一次被用户拒绝的
    /// 危险建议在审计里永远显示为已批准，且 `Verdict::executed()` 对它返回真——
    /// 链在密码学上完整、在语义上是错的。
    ///
    /// 用源码扫描而非行为测试：`audit_ai` 要 Tauri State + 真库，而这条不变量
    /// 说的是「那个 match 的哪一臂映到哪一档」，读得出来就够。
    #[test]
    fn a_suggestion_awaiting_confirmation_is_never_recorded_as_executed() {
        let src = include_str!("ai_cmd.rs");
        let prod = match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        };
        // 确认档必须映到 RequestOnly（发起了、未执行）
        assert!(
            prod.contains("fs_connmgr::audit_repo::Verdict::RequestOnly"),
            "待确认的建议要记 RequestOnly"
        );
        // 且**不得**出现 Approved——这一路没有任何时刻是「已批准」
        assert!(
            !prod.contains("fs_connmgr::audit_repo::Verdict::Approved"),
            "建议路径上不该出现 Approved：用户还没点，执行结果由执行路径另记一条"
        );
        // 做空防护：扫的是真源码（Denied 那一臂还在）
        assert!(prod.contains("fs_connmgr::audit_repo::Verdict::Denied"));
    }

    /// 语义对照：`RequestOnly` 确实不算「执行过」，而 `Approved` 算。
    ///
    /// 上一条钉的是「代码里写的是哪一档」，这条钉的是「那一档的语义确实是我们
    /// 要的」——两条缺一：只有前者的话，哪天 RequestOnly 的 executed() 改成真，
    /// 代码没动、缺陷却回来了。
    #[test]
    fn request_only_does_not_count_as_executed_but_approved_does() {
        use fs_connmgr::audit_repo::Verdict;
        assert!(!Verdict::RequestOnly.executed());
        assert!(Verdict::Approved.executed());
        assert!(!Verdict::Denied.executed());
        assert!(!Verdict::Rejected.executed());
    }
}
