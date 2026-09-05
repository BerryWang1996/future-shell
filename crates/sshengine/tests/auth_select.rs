use fs_sshengine::auth::{
    next_method, prioritize_agent_keys, suppressed_by_config, CredentialSet, Method,
};
use zeroize::Zeroizing;

/// 四种方法全配齐、服务器全通告——用来观察**次序**本身。
fn everything() -> CredentialSet {
    CredentialSet::builder()
        .password(Zeroizing::new("pw".into()))
        .kbd_interactive()
        .public_key(Zeroizing::new("pem".into()))
        .agent()
        .build()
}

/// 把 `next_method` 反复调用到 None，得到本会话**实际的尝试次序**。
///
/// 逐条 `assert_eq!(next_method(&creds, &[…前缀…], &all), Some(X))` 也能表达同一件事，
/// 但那种写法把「次序」摊成了五条互不相干的断言：改动次序时它们会一起变红，红在哪一步
/// 却要人去拼；而拼出来的次序又正是被改坏的那个。收成一个 Vec 之后，断言的形状与
/// 被断言的性质是同一个东西，失败信息直接把两条次序摆在一起。
fn attempt_order(creds: &CredentialSet, announced: &[Method]) -> Vec<Method> {
    let mut tried = Vec::new();
    while let Some(m) = next_method(creds, &tried, announced) {
        assert!(
            !tried.contains(&m),
            "next_method 重复给出 {m}，会白占 MaxAuthTries"
        );
        tried.push(m);
    }
    tried
}

#[test]
fn never_tries_unannounced_method() {
    let creds = CredentialSet::builder()
        .password(Zeroizing::new("pw".into()))
        .agent()
        .build();
    // 服务器只通告 publickey：手里有口令也不得尝试（偏好序是次序，通告是可不可以）
    let m = next_method(&creds, &[], &[Method::PublicKey, Method::Agent]);
    assert!(matches!(m, Some(Method::Agent) | Some(Method::PublicKey)));
    assert_ne!(m, Some(Method::Password));
}

/// 审计2 #26：**密钥类在前，口令在最后**，且每种方法至多试一次。
///
/// 这不是审美偏好。原序是 `[Password, KbdInteractive, PublicKey, Agent]`，代价有两笔：
///
/// ① 一台既通告 password 又通告 publickey 的服务器（绝大多数 sshd 的默认配置就是这样），
///    会先收到用户的明文口令；而这次交付本可以完全不发生——后面就排着一把能过的密钥。
/// ② 每次失败白占一次 `MaxAuthTries`。堡垒机常配 2–3 次，口令与 kbd-interactive 两次
///    失败就足以让密钥**永远排不到**，用户看到的现象是「明明配了密钥却登不上」。
///
/// 断言写完整的四元次序而不只是「Password 不在第一」：只钉第一名的话，把次序改成
/// `[PublicKey, Password, Agent, KbdInteractive]` 照样绿，而那个次序仍然会在密钥失败后
/// 立刻把口令送出去，②那笔代价一分没少。
#[test]
fn key_based_methods_are_tried_before_the_password_is_ever_sent() {
    let all = [
        Method::Password,
        Method::KbdInteractive,
        Method::PublicKey,
        Method::Agent,
    ];
    assert_eq!(
        attempt_order(&everything(), &all),
        vec![
            Method::PublicKey,
            Method::Agent,
            Method::KbdInteractive,
            Method::Password
        ],
        "口令必须排在所有密钥类方法之后：排在前面等于把明文口令送给一台可能根本不收它的服务器，\
         还白占掉密钥所需的 MaxAuthTries 名额"
    );
}

/// 通告集合的**任何**子集下，口令都不得排在某个可用的密钥类方法之前。
///
/// 上一条钉的是全通告这一种情形；服务器的通告是它说了算的，真实世界里 16 种子集都可能出现。
/// 逐个子集验一遍，杜绝「只在四个都通告时才对」的次序。
#[test]
fn password_is_last_under_every_announcement_subset() {
    let all = [
        Method::Password,
        Method::KbdInteractive,
        Method::PublicKey,
        Method::Agent,
    ];
    let creds = everything();
    for mask in 0u8..16 {
        let announced: Vec<Method> = all
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, m)| *m)
            .collect();
        let order = attempt_order(&creds, &announced);
        let Some(pw_at) = order.iter().position(|m| *m == Method::Password) else {
            continue;
        };
        for (i, m) in order.iter().enumerate() {
            if matches!(m, Method::PublicKey | Method::Agent) {
                assert!(
                    i < pw_at,
                    "通告 {announced:?} 下的次序 {order:?} 把口令排在了 {m} 之前"
                );
            }
        }
    }
}

/// 服务器禁 password、仅通告 kbd-interactive（OpenSSH+PAM 常见）时仍走得通。
#[test]
fn open_ssh_password_via_kbd_interactive_fallback() {
    let creds = CredentialSet::builder()
        .password(Zeroizing::new("pw".into()))
        .kbd_interactive()
        .build();
    let m = next_method(&creds, &[], &[Method::KbdInteractive]);
    assert_eq!(m, Some(Method::KbdInteractive));
}

/// 审计2 #19：**关掉 keyboard-interactive 就是关掉**，配没配口令都一样。
///
/// 旧判据是 `creds.kbd_interactive || creds.password.is_some()`。那个 `||` 让这个开关在
/// 「配了口令」时恒为空操作——而配了口令正是绝大多数用户的情形，于是开关基本等于不存在。
/// 审计把它点名为「明确的安全控制失效」，理由不在于 keyboard-interactive 本身有多危险，
/// 而在于**提示语由服务器写、答案由客户端交**：一台被攻陷的服务器可以把提示写成
/// 「请输入您的域账号口令以继续」，诱导出与这次 SSH 登录无关的秘密。关掉它正是为了不参与。
///
/// 注意上面那条 `open_ssh_password_via_kbd_interactive_fallback` **抓不到**这个缺陷：
/// 它自己调了 `.kbd_interactive()`，`||` 的左边已经为真，右边是死是活它都绿。
/// 一个从未被任何用例触及的分支，就是这样活了下来。
#[test]
fn kbd_interactive_switch_is_not_overridden_by_having_a_password() {
    let creds = CredentialSet::builder()
        .password(Zeroizing::new("pw".into()))
        .build(); // 显式：未调用 .kbd_interactive()

    assert_eq!(
        next_method(&creds, &[], &[Method::KbdInteractive]),
        None,
        "配置里关掉了 keyboard-interactive，持有口令不构成打开它的理由"
    );
    // 服务器同时通告两者时，走的是明确开着的那一条，而不是被关掉的那一条
    assert_eq!(
        attempt_order(&creds, &[Method::Password, Method::KbdInteractive]),
        vec![Method::Password]
    );
}

/// #19 修好之后的配套：为什么连不上，必须说得出来。
///
/// 把开关修成真的会关，一部分「只认 keyboard-interactive 口令」的服务器就会从
/// 「悄悄用上口令、连上了」变成「认证失败」。修得对，但用户若只看到一句「认证失败」，
/// 这个开关就成了一个无法自行诊断的陷阱。
#[test]
fn suppressed_method_is_reported_only_when_it_actually_cost_something() {
    let with_pw = CredentialSet::builder()
        .password(Zeroizing::new("pw".into()))
        .build();
    assert_eq!(
        suppressed_by_config(&with_pw, &[Method::KbdInteractive]),
        vec![Method::KbdInteractive],
        "服务器还通告着它、手里也有口令，却因为配置没去试——这正是用户需要被告知的那一句"
    );

    // 没配口令：关掉它什么也没压掉（本来就无料可喂）。报出来是噪声，而噪声会淹掉真正的提示。
    let no_pw = CredentialSet::builder().agent().build();
    assert!(suppressed_by_config(&no_pw, &[Method::KbdInteractive]).is_empty());

    // 开关是开的：没有任何东西被压制
    let on = CredentialSet::builder()
        .password(Zeroizing::new("pw".into()))
        .kbd_interactive()
        .build();
    assert!(suppressed_by_config(&on, &[Method::KbdInteractive]).is_empty());

    // 服务器压根没通告它：说「你关掉了它」是误导，用户就算打开也连不上
    assert!(suppressed_by_config(&with_pw, &[Method::Password]).is_empty());
}

#[test]
fn agent_keys_never_repeat_an_identity() {
    // S24：同一身份在 available 里出现两次（多 agent 复用 / 转发链 / 非 OpenSSH agent
    // 不去重时可能发生）。历史实现的「偏好优先」阶段只 filter 不去重，命中的偏好身份会被
    // 推两次；其后的补齐阶段虽有 `!out.contains(k)`，却只管自己新推的，救不回来。
    // 后果正是 cap 存在的理由被架空：同一把密钥白占两个 cap 名额、白耗服务器两次
    // MaxAuthTries，真正能过的密钥反而排不进来。
    let available = vec!["d".into(), "d".into(), "a".into(), "b".into()];
    let preferred = vec!["d".into()];
    let chosen = prioritize_agent_keys(&available, &preferred, 3);
    assert_eq!(
        chosen,
        vec!["d".to_string(), "a".to_string(), "b".to_string()]
    );
}

#[test]
fn agent_keys_prioritized_and_capped() {
    let available = vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()];
    let preferred = vec!["d".into()];
    let chosen = prioritize_agent_keys(&available, &preferred, 3);
    assert_eq!(
        chosen,
        vec!["d".to_string(), "a".to_string(), "b".to_string()]
    );
}
