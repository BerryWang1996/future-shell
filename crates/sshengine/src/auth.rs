use std::fmt;
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Password,
    KbdInteractive,
    PublicKey,
    /// **线上没有这个方法**：agent 认证在协议层就是 `publickey`，只是密钥材料来自 agent 而非
    /// 本地 PEM。本变体是纯本地区分，故 [`Method::from_wire`] 刻意无 `"agent"` 分支。
    ///
    /// 由此得出调用方的一条硬约束（S25）：**凡是从服务器通告构造 `server_remaining` 的地方，
    /// 都必须在 `publickey` 在列且本会话启用了 agent 时补回 `Method::Agent`**，否则
    /// [`next_method`] 永远选不到它，agent 认证整条路**静默失效** —— 不报错、不告警，
    /// 只是「agent 里明明有可用密钥却总是登不上」。本模块的单元测试结构上抓不到这个违例
    /// （它们直接以字面量传 `server_remaining`，绕过了通告映射），故约束写在此处。
    /// 现有落点：`connect.rs` 认证循环收到 `AuthStep::Failure` 后重建 `remaining` 之处。
    Agent,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Method::Password => "password",
            Method::KbdInteractive => "keyboard-interactive",
            Method::PublicKey => "publickey",
            Method::Agent => "agent",
        })
    }
}

impl Method {
    /// 从 russh/服务器通告名称解析（RFC 4252）。
    ///
    /// **与 [`Display`](fmt::Display) 不是互逆的**：`Method::Agent` 显示为 `"agent"`（仅用于
    /// 面向用户的错误文案），但线上无此方法，故这里不接受它 —— 详见 [`Method::Agent`] 上
    /// 关于「调用方必须自行补回 Agent 候选」的约束（S25）。
    pub fn from_wire(name: &str) -> Option<Method> {
        match name {
            "password" => Some(Method::Password),
            "keyboard-interactive" => Some(Method::KbdInteractive),
            "publickey" => Some(Method::PublicKey),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct CredentialSet {
    /// Zeroizing<String>：随 drop 自动清零——秘密不以裸 String 跨 await / 出函数边界（总设计 §3.2）
    pub password: Option<Zeroizing<String>>,
    pub kbd_interactive: bool,
    pub public_key_pem: Option<Zeroizing<String>>,
    pub agent: bool,
}

/// 手写 Debug 取代 derive：敏感字段恒输出 Some("[REDACTED]")/None，禁经调试打印泄出（总设计 §3.2）
impl fmt::Debug for CredentialSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialSet")
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("kbd_interactive", &self.kbd_interactive)
            .field(
                "public_key_pem",
                &self.public_key_pem.as_ref().map(|_| "[REDACTED]"),
            )
            .field("agent", &self.agent)
            .finish()
    }
}

impl CredentialSet {
    pub fn builder() -> Builder {
        Builder::default()
    }
}

#[derive(Default)]
pub struct Builder(CredentialSet);
impl Builder {
    /// 秘密入参一律 Zeroizing<String>：调用方显式包裹，杜绝裸 String 进入凭据集
    pub fn password(mut self, p: Zeroizing<String>) -> Self {
        self.0.password = Some(p);
        self
    }
    pub fn kbd_interactive(mut self) -> Self {
        self.0.kbd_interactive = true;
        self
    }
    pub fn public_key(mut self, pem: Zeroizing<String>) -> Self {
        self.0.public_key_pem = Some(pem);
        self
    }
    pub fn agent(mut self) -> Self {
        self.0.agent = true;
        self
    }
    pub fn build(self) -> CredentialSet {
        self.0
    }
}

/// 尝试次序（审计2 #26）。**密钥类在前，口令在最后。**
///
/// 原序把 `Password` 排在第一，后果不是理论上的（`connect.rs` 的认证循环里也记着这一条）：
///
/// - 「配了口令 + 配了密钥」的 profile 连一台 publickey-only 的服务器时，明文口令会被送给
///   一台**永远不会接受它**的服务器。通道是加密的，但对端拿到了口令，而这次交付毫无必要——
///   本来就有一把能过的密钥排在后面。
/// - 每次失败白占一次 `MaxAuthTries`。堡垒机常配 2–3 次，四个候选方法足以在密钥被试到之前
///   直接把连接锁死，用户看到的现象是「明明配了密钥却登不上」。
///
/// `PublicKey` 排在 `Agent` 之前：前者是用户为**这个 profile** 显式指定的一把，后者是 agent
/// 里可能有几十把的扫射，每把都吃掉一次 `MaxAuthTries`（`AGENT_IDENTITY_CAP_DEFAULT` 只是
/// 把扫射的宽度压到 3，不能把它变成零）。先试指名的那一把，命中率与代价都更优。
///
/// `KbdInteractive` 排在 `Password` 之前：同为口令交付，但很多服务器把 OTP/二次验证挂在
/// keyboard-interactive 上，这一档能过的场景严格更多，而失败的代价与 password 相同。
const PREFERENCE: [Method; 4] = [
    Method::PublicKey,
    Method::Agent,
    Method::KbdInteractive,
    Method::Password,
];

/// 某个方法是否**有料可试**：本会话是否持有能喂给它的凭据。
///
/// 独立成函数而不是内联进 [`next_method`] 的闭包，是为了让 [`suppressed_by_config`] 与它
/// 共用同一份判据——「哪些方法有料」若在两处各写一遍，「因配置被压掉」的名单迟早与
/// 实际尝试的名单对不上，而那份名单是要直接说给用户听的。
fn has_material(creds: &CredentialSet, m: Method) -> bool {
    match m {
        Method::Password => creds.password.is_some(),
        // 审计2 #19：这里曾是 `creds.kbd_interactive || creds.password.is_some()`。
        // 那个 `||` 让「关闭 keyboard-interactive」这个开关在**配了口令时恒为空操作**——
        // 而配了口令正是绝大多数用户的情形，于是这个开关基本等于不存在。
        //
        // 它不是可有可无的偏好项：keyboard-interactive 由服务器**发提示、由客户端回答**，
        // 一台恶意或被攻陷的服务器可以借它把提示语写成任意文本（"请输入您的域账号口令以继续"），
        // 诱导出与本次 SSH 登录无关的秘密；关掉它正是为了不参与这种交互。
        // 一个安全开关必须**说关就关**，否则它比没有更糟：用户以为自己关了。
        Method::KbdInteractive => creds.kbd_interactive,
        Method::PublicKey => creds.public_key_pem.is_some(),
        Method::Agent => creds.agent,
    }
}

/// 服务器通告驱动：偏好序 ∩ 服务器通告 ∩ 未尝试（spec §2.1）。
pub fn next_method(
    creds: &CredentialSet,
    tried: &[Method],
    server_remaining: &[Method],
) -> Option<Method> {
    PREFERENCE
        .into_iter()
        .find(|m| has_material(creds, *m) && server_remaining.contains(m) && !tried.contains(m))
}

/// 服务器**仍在通告**、本会话**握有可喂的材料**、却因为配置开关而不去试的方法。
///
/// 存在的理由是 #19 的直接副作用：把 keyboard-interactive 开关修成真的会关，
/// 一部分「只认 keyboard-interactive 口令」的服务器就会从「悄悄用上口令、连上了」
/// 变成「认证失败」。修得对，但若用户只看到一句「认证失败」，这个开关就成了一个
/// **无法自行诊断**的陷阱。故把它变成一句写进 `Error::Auth.notes` 的可行动提示。
///
/// 判据里 `password.is_some()` 这一条不能少：没配口令时关掉 keyboard-interactive
/// 什么也没压掉（本来就无料可喂），报出来纯属噪声，而噪声会让真正有用的那条被淹掉。
///
/// 今天只有 keyboard-interactive 一档会出现在返回值里——`allow_agent=false` 不算，
/// 因为客户端无从得知机器上到底有没有在跑 agent，报「你关掉了 agent」既可能是废话
/// 也可能是误导。宁可少说一句，不可说一句猜的。
pub fn suppressed_by_config(creds: &CredentialSet, server_remaining: &[Method]) -> Vec<Method> {
    let mut out = Vec::new();
    if server_remaining.contains(&Method::KbdInteractive)
        && !creds.kbd_interactive
        && creds.password.is_some()
    {
        out.push(Method::KbdInteractive);
    }
    out
}

/// 「一条都没试、且一条材料都没有」时该给用户的那句可行动提示。
///
/// 与 [`suppressed_by_config`] 是**互斥的两类失败**，修法完全不同，必须分开说：
/// - 有材料但被开关压掉 → 去开开关（`suppressed_by_config` 的那句）；
/// - 一条材料都没有 → 去配凭据（本函数这句）。
///
/// 若把两者并成一句「认证失败」，用户会去开关页瞎找，而真正的缺口（根本忘了存口令）
/// 无人提示——正是「填了主机信息、双击连接、直接报错、不弹密码框」那一类现场。
pub fn credentialless_note(
    creds: &CredentialSet,
    tried: &[Method],
    server_remaining: &[Method],
) -> Option<&'static str> {
    if !tried.is_empty() {
        return None; // 试过但没过：是「试了没过」，不是「没料可试」
    }
    let any = [
        Method::Password,
        Method::PublicKey,
        Method::Agent,
        Method::KbdInteractive,
    ]
    .iter()
    .any(|m| has_material(creds, *m));
    if any {
        return None; // 有料但没试成（被开关压掉/服务器未通告）→ 交给 suppressed_by_config 说
    }
    // 按**服务器通告的方法**分岔（2026-08-31 用户真机报出）。
    //
    // 原来只有一句话，开头就是「请存入口令」。可当服务器通告里根本没有 password
    // （`PasswordAuthentication no`，云主机的常见配置），那句话把用户引向死路：
    // 他去存了口令，回来照样连不上——这台机器压根不收口令。
    //
    // 只陈述**对端通告的事实**，不猜本机状态（守 `suppressed_by_config` 文档里
    // 那条纪律：不说「你机器上有 agent 没用」，客户端无从得知）。publickey-only
    // 那句把 Agent 放第一位：它是唯一不需要先建保险库的当场可行项。
    let pubkey_only = server_remaining.contains(&Method::PublicKey)
        && !server_remaining.contains(&Method::Password)
        && !server_remaining.contains(&Method::KbdInteractive);
    if pubkey_only {
        return Some(
            "本连接未配置任何可用的认证凭据，而这台服务器只接受密钥认证（publickey）\u{2014}\u{2014}存口令没有用：\
             请在「会话属性 → 认证」页勾选「使用本地 SSH Agent 认证」（若已用 ssh-add 加载过密钥），\
             或在「凭据」里选「使用保险库中的私钥」并挑一条私钥记录后重试",
        );
    }
    Some(
        "本连接未配置任何可用的认证凭据（口令、私钥、Agent 均无）：请在「会话属性 → 认证」页 \
        存入口令（存入 Vault…）或选择一条凭据记录，或勾选「使用本地 SSH Agent 认证」后重试",
    )
}

/// 「持有的凭据类型与服务器要求的**不匹配**」——一次认证都没发起时的第三类失败。
///
/// 补的是 [`credentialless_note`] 与 [`suppressed_by_config`] 之间的缝隙（S303）。
/// 三类「`tried` 为空」的失败，修法各不相同：
/// - 一条材料都没有            → 去配凭据（`credentialless_note`）
/// - 有材料但被开关压掉        → 去开开关（`suppressed_by_config`）
/// - **有材料，但与服务器要的不是一类** → 去配**服务器要的那一类**（本函数）
///
/// 第三类此前无人认领，而它恰是最常见的现场之一：**配了口令、服务器只收 publickey**。
/// 用户看到的是 `auth failed; tried: []; server allows: ["publickey"]; notes: []`——
/// 一次都没试、也没有任何一句说明为什么，"tried 为空"这个最关键的线索还得懂协议才读得懂。
///
/// 判据说明：`tried` 为空且持有材料，即证明「持有集 ∩ 服务器通告 = ∅」——若有交集，
/// [`next_method`] 必返回 `Some`，`tried` 就不会是空的。仍显式复核该交集为空再出声：
/// 这句话点名了双方，说错比不说更糟。
///
/// `suppressed_by_config` 非空时让位：那句更具体（点到了具体哪个开关），
/// 两句并列只会稀释掉可行动的那条。
pub fn material_mismatch_note(
    creds: &CredentialSet,
    tried: &[Method],
    server_remaining: &[Method],
) -> Option<String> {
    if !tried.is_empty() {
        return None; // 试过但没过：是「试了没过」，不是「配错了类型」
    }
    if !suppressed_by_config(creds, server_remaining).is_empty() {
        return None; // 更具体的那句已覆盖
    }
    let held: Vec<Method> = PREFERENCE
        .into_iter()
        .filter(|m| has_material(creds, *m))
        .collect();
    if held.is_empty() {
        return None; // 一条材料都没有 → credentialless_note 管
    }
    // 服务器侧只报**线上真实存在**的方法：`Method::Agent` 是本地合成的（协议层即
    // publickey，见 Method::Agent 文档），把它当成「服务器要求 agent」会误导用户。
    let wire: Vec<Method> = server_remaining
        .iter()
        .copied()
        .filter(|m| *m != Method::Agent)
        .collect();
    if wire.is_empty() {
        return None; // 无从对比（服务器没通告任何方法）
    }
    if held.iter().any(|m| server_remaining.contains(m)) {
        return None; // 有交集 ⇒ 未发起另有其因，不可断言为类型不匹配
    }
    let list = |ms: &[Method]| {
        ms.iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join("、")
    };
    let mut msg = format!(
        "服务器只接受 {}，而本连接可用的凭据是 {}：两者无交集，故一次认证都未发起（tried 为空）。\
         请在「会话属性 → 认证」页改配服务器要求的凭据类型",
        list(&wire),
        list(&held)
    );
    if wire.contains(&Method::PublicKey) {
        msg.push_str(
            "——publickey 需要在「凭据」里选「使用保险库中的私钥」并挑一条私钥记录（私钥若已加密，再选一条密码类记录作「私钥口令」），\
             或勾选「使用本地 SSH Agent 认证」并确保本机 agent 已 ssh-add 过对应密钥",
        );
    }
    Some(msg)
}

pub const AGENT_IDENTITY_CAP_DEFAULT: usize = 3;

/// Produces `auth::agent_identities_cap` 声明的载体：返回默认上限常量。
pub fn agent_identities_cap() -> usize {
    AGENT_IDENTITY_CAP_DEFAULT
}

/// 审计2 #36：档案 `kbd_auto_answer_single`（单提示自动应答）的判定纯函数。
///
/// 全部满足才应答，任一不满足返回 `None`（正常走弹框）：
/// - 开关开（`flag`）；
/// - **恰一个**提示——多于一个时服务器在问多件事，客户端无从知道哪一件该吃口令，
///   自动应答即盲猜（恰是审计原文对「凭据用途靠猜」的批评）；
/// - 该提示 `echo == false`——`echo=true` 的提示是**新秘密录入**（改密/注册/OATH 绑定），
///   自动填旧口令轻则失败、重则把旧口令提交进不该进的地方；
/// - 配了口令——没有口令无从应答。
///
/// 返回的是**答案本身**；提示文本一律不进日志/状态栏（P1-19/20 口径由调用方维持）。
pub fn kbd_auto_answer(
    flag: bool,
    prompts: &[crate::events::Prompt],
    password: Option<&str>,
) -> Option<String> {
    if !flag {
        return None;
    }
    let [only] = prompts else {
        return None;
    };
    if only.echo {
        return None;
    }
    password.map(str::to_string)
}

/// 匹配 profile 配置/历史接受者优先；总数受 cap 限制（防耗尽 MaxAuthTries）。
///
/// 两个阶段都必须去重（S24）。`available` 里同一身份出现两次并非不可能（多 agent 复用、
/// 转发链、非 OpenSSH 实现不做 add 去重），而一旦它又落在 `preferred` 里，只 filter 不去重
/// 就会把它推两次：cap 名额与服务器 MaxAuthTries 各被同一把密钥白占两份，真正能过的密钥
/// 反而排不进来 —— 恰好架空了 cap 存在的理由。补齐阶段的 `!out.contains(k)` 救不回来，
/// 它只管自己新推的那些。
pub fn prioritize_agent_keys(
    available: &[String],
    preferred: &[String],
    cap: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in available {
        if preferred.contains(k) && !out.contains(k) {
            out.push(k.clone());
        }
    }
    for k in available {
        if out.len() >= cap {
            break;
        }
        if !out.contains(k) {
            out.push(k.clone());
        }
    }
    out.truncate(cap);
    out
}
