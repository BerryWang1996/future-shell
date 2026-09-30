//! 远端进程列表与终止（M4a：服务器进程管理，FinalShell 对标）。
//!
//! 与 [`crate::monitor`] 同一套路：**命令是常量、解析是纯函数**，都能离线钉住；
//! 真正跑命令的是 app 层经 `exec_adapter_for` 拿到的 `ExecChannel`（已套总超时）。
//!
//! ## 为什么用 `ps -eo` 的固定字段序，而不是 `ps aux`
//!
//! `ps aux` 的列顺序与含义随实现而变（GNU/BSD/busybox 各不同），而且 `USER` 列会被
//! 截断成 8 字符加个 `+`。`-o` 显式点名字段并给宽度，是唯一能让解析器**知道自己在
//! 解析什么**的写法。末尾必须是 `args`（含空格），所以解析按「前 N 段定长切、余下
//! 全归命令」来做。
//!
//! ## 杀进程的注入面：没有
//!
//! `pid` 是 `u32`，信号来自**闭合白名单**（映射到写死的字面量），二者拼进命令时都
//! 不可能带出 shell 元字符。这一条是刻意的设计而不是碰巧——一个「用户填什么就拼什么」
//! 的 kill 命令等于把远端 shell 交出去。

/// 一条进程记录。
///
/// 数值字段一律 `Option`：`ps` 在某些实现上会对内核线程等给出 `-` 或空白，
/// 解析不出来就落空态由 UI 显示「—」。**绝不用 0 冒充「未知」**——0% CPU 是一个
/// 有意义的观测值，而「不知道」不是。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub user: String,
    /// CPU 占用百分比（`ps` 的 pcpu：进程生命周期内的均值，不是瞬时值）
    pub cpu: Option<f64>,
    /// 内存占用百分比（pmem）
    pub mem: Option<f64>,
    /// 常驻集大小，KiB（rss）
    pub rss_kb: Option<u64>,
    /// 进程状态（`stat`，如 `S`/`R`/`Z`/`Ssl`）
    pub state: String,
    /// 完整命令行（含参数）
    pub command: String,
}

/// 取进程列表的命令。
///
/// - `-e` 全部进程；`-o` 显式字段序。
/// - `user:32` 给足宽度，否则长用户名被截成 `systemd+` 这种带 `+` 的残名。
/// - 字段名后跟 `=` 抑制表头：**不能靠跳过第一行**来去表头，某些实现（busybox）
///   压根不打表头，跳一行就吃掉一个真进程。
/// - `args` 放最后（唯一含空格的字段）。
/// - `--no-headers` 之类的 GNU 专有开关一概不用，保持 POSIX 面。
/// - 回落式 `||`：busybox 的 `ps` 不认 `-o` 的宽度语法，去掉宽度再试一次。
///
/// **刻意不加 `2>/dev/null`**：stderr 正是 [`classify_empty`] 判「远端根本不支持」
/// 的依据，掐掉它就只剩一个空列表，而空列表读起来像「这台机器上没有进程」。
/// 回落成功时 stderr 里会留着第一条命令的抱怨，那不影响判定——`classify_empty`
/// 只在**一行都没解析出来**时才看 stderr。
pub fn process_list_command() -> &'static str {
    "ps -eo pid=,ppid=,user:32=,pcpu=,pmem=,rss=,stat=,args= || ps -eo pid=,ppid=,user=,pcpu=,pmem=,rss=,stat=,args="
}

/// 解析 [`process_list_command`] 的 stdout。
///
/// 逐行切前 7 段、余下全归命令。解析不出 `pid` 的行**整行丢弃**（表头、告警、
/// 空行都长这样），其余字段各自落空态。绝不 panic：输出来自远端，形状不可尽信。
pub fn parse_process_list(stdout: &str) -> Vec<ProcessInfo> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim_start();
        if line.is_empty() {
            continue;
        }
        // 前 7 段按空白切、余下原样归命令。不用 `splitn(8, whitespace)`：`ps` 用
        // 空格右对齐补位，连续空白会切出一堆空段，段数与 `splitn` 的界对不上。
        if let Some(rec) = split_fields(line) {
            out.push(rec);
        }
    }
    out
}

/// 把一行切成 7 个字段 + 命令。失败（含 pid 解析不出）返回 `None`。
fn split_fields(line: &str) -> Option<ProcessInfo> {
    let mut rest = line;
    let mut fields: Vec<&str> = Vec::with_capacity(7);
    for _ in 0..7 {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace)?;
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    let command = rest.trim_start();
    let pid: u32 = fields[0].parse().ok()?;
    Some(ProcessInfo {
        pid,
        ppid: fields[1].parse().ok(),
        user: fields[2].to_string(),
        cpu: fields[3].parse().ok(),
        mem: fields[4].parse().ok(),
        rss_kb: fields[5].parse().ok(),
        state: fields[6].to_string(),
        command: command.to_string(),
    })
}

/// 允许发送的信号。
///
/// 闭合白名单而不是「用户填什么发什么」：一是消掉命令拼接的注入面，二是这几个
/// 之外的信号（`SIGSTOP` 之类）在运维场景里要么无用、要么会把服务挂成半死不活
/// 而看起来还在跑。要发别的信号，用户在终端里敲 `kill` 更直接、也更清楚自己在做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// 请求优雅退出（默认）
    Term,
    /// 重载配置（多数守护进程的惯例）
    Hup,
    /// 相当于 Ctrl-C
    Int,
    /// 强杀，不可被捕获——**丢数据的那一个**
    Kill,
}

impl Signal {
    /// 从前端字符串解析。未知值返回 `None`（**不回落到 TERM**：把一个拼错的信号
    /// 名当成 TERM 是在替用户猜，而猜错的后果是杀掉了他没打算杀的东西）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "TERM" | "SIGTERM" | "15" => Some(Self::Term),
            "HUP" | "SIGHUP" | "1" => Some(Self::Hup),
            "INT" | "SIGINT" | "2" => Some(Self::Int),
            "KILL" | "SIGKILL" | "9" => Some(Self::Kill),
            _ => None,
        }
    }

    /// `kill -s` 的实参。返回 `&'static str` 是这条设计的关键：拼进命令的永远是
    /// 编译期字面量，用户输入无从抵达命令串。
    pub fn as_flag(self) -> &'static str {
        match self {
            Self::Term => "TERM",
            Self::Hup => "HUP",
            Self::Int => "INT",
            Self::Kill => "KILL",
        }
    }

    /// 是否是「会丢数据」的那一类（UI 据此加重确认文案）。
    pub fn is_destructive(self) -> bool {
        matches!(self, Self::Kill)
    }
}

/// 杀进程会被拒绝的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillReject {
    /// pid 0 在 POSIX 里是「当前进程组的全部进程」——一次误填能把整组打掉
    PidZero,
    /// 未知信号名
    UnknownSignal(String),
}

impl std::fmt::Display for KillReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PidZero => write!(f, "PID 0 在 POSIX 里表示「当前进程组的全部进程」，拒绝执行"),
            Self::UnknownSignal(s) => {
                write!(f, "不支持的信号「{s}」（可用：TERM / HUP / INT / KILL）")
            }
        }
    }
}

/// 构造 kill 命令。
///
/// **不拦 PID 1**：容器里 1 号进程常常正是用户要重启的那个服务，一刀切禁掉就把
/// 一个正当用法堵死了。取而代之的是 [`needs_loud_confirm`]——由 UI 把话说清楚，
/// 让人自己决定。拒绝只留给「无法表达合理意图」的输入（PID 0）。
pub fn kill_command(pid: u32, signal: &str) -> Result<String, KillReject> {
    if pid == 0 {
        return Err(KillReject::PidZero);
    }
    let sig = Signal::parse(signal).ok_or_else(|| KillReject::UnknownSignal(signal.to_string()))?;
    // pid 是 u32、sig 是字面量：命令串里不存在用户可控的自由文本
    Ok(format!("kill -s {} {}", sig.as_flag(), pid))
}

/// 该操作是否值得一句「重话」确认（UI 用）。
///
/// PID 1 是 init/容器主进程，杀掉通常等于整机或整容器停摆；`KILL` 不可被捕获，
/// 进程来不及落盘。两者都不阻止，但都必须让用户看见后果。
pub fn needs_loud_confirm(pid: u32, signal: Signal) -> bool {
    pid == 1 || signal.is_destructive()
}

/// 判断「远端根本不支持这套采集」还是「确实没解析出东西」。
///
/// 空列表必须与失败区分开：把执行失败显示成一个空进程表，用户读到的是「这台机器
/// 上没有进程」——一个永远不可能为真的结论，却看起来像正常结果。
pub fn classify_empty(exit_code: Option<i32>, stdout: &str, stderr: &str) -> Option<String> {
    if !parse_process_list(stdout).is_empty() {
        return None;
    }
    let err = stderr.trim();
    // None（超时/通道异常）与非零一样是失败，且必须把「没拿到退出码」说出来
    if exit_code != Some(0) || !err.is_empty() {
        return Some(format!(
            "远端进程采集失败（exit {}）：{}",
            match exit_code {
                Some(c) => c.to_string(),
                None => "未观测到（超时或通道异常）".to_string(),
            },
            if err.is_empty() {
                "无 stderr 输出；该系统可能没有 POSIX 版 ps".to_string()
            } else {
                err.to_string()
            }
        ));
    }
    Some("远端 ps 没有输出可解析的进程行（该系统可能不提供 POSIX 版 ps）".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 典型 Linux `ps -eo` 输出：字段各就各位，命令保留内部空格。
    #[test]
    fn parses_typical_linux_output() {
        let out = "\
    1        0 root                              0.0  0.1  13245 Ss   /sbin/init splash
  842        1 systemd-resolve                   0.3  0.4  22100 Ssl  /lib/systemd/systemd-resolved
 1337      842 alice                            12.5  3.2 331200 R+   python3 -u train.py --epochs 30
";
        let procs = parse_process_list(out);
        assert_eq!(procs.len(), 3);
        assert_eq!(procs[0].pid, 1);
        assert_eq!(procs[0].ppid, Some(0));
        assert_eq!(procs[0].user, "root");
        assert_eq!(procs[0].command, "/sbin/init splash", "命令须保留内部空格");
        // 长用户名不得被截断（`-o user:32` 的理由）
        assert_eq!(procs[1].user, "systemd-resolve");
        assert_eq!(procs[2].cpu, Some(12.5));
        assert_eq!(procs[2].mem, Some(3.2));
        assert_eq!(procs[2].rss_kb, Some(331_200));
        assert_eq!(procs[2].state, "R+");
        assert_eq!(procs[2].command, "python3 -u train.py --epochs 30");
    }

    /// 表头行、告警行、空行一律丢弃——而**不是**「跳过第一行」。
    ///
    /// busybox 的 ps 压根不打表头，跳一行就吃掉一个真进程；反过来，某些实现会在
    /// stdout 里混一句警告。按「pid 解析不出即丢弃」判定对两种情形都成立。
    #[test]
    fn drops_non_process_lines_without_skipping_first() {
        let out = "\
  PID  PPID USER                             %CPU %MEM   RSS STAT COMMAND
Warning: /proc not fully mounted
    7        1 root                              0.0  0.0   900 S    /usr/bin/dash

";
        let procs = parse_process_list(out);
        assert_eq!(procs.len(), 1, "只有一行是真进程，实得 {procs:?}");
        assert_eq!(procs[0].pid, 7);

        // 关键的另一半：**无表头**（busybox 的 ps 不打表头）时，第一行就是真进程，
        // 一个都不能少。只测「有表头」那一半的话，把实现改成 `.skip(1)` 也照过
        // ——变异验证实测如此，而那个改动恰恰就是本测试名要否掉的写法。
        let headerless = "\
    1        0 root                              0.0  0.1   700 S    /sbin/init
   23        1 nobody                            0.0  0.0   400 S    /usr/sbin/crond
";
        let procs = parse_process_list(headerless);
        assert_eq!(
            procs.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![1, 23],
            "无表头时第一行也是真进程，不得被跳过"
        );
    }

    /// 数值字段缺失（内核线程给 `-`）落空态，**不落 0**。
    ///
    /// 0% CPU 是有意义的观测值，「不知道」不是。用 0 冒充未知会让 UI 上出现一堆
    /// 看起来精确的假数据。
    #[test]
    fn missing_numeric_fields_become_none_not_zero() {
        let out =
            "   42        2 root                                -    -     - I<   [kworker/0:1H]\n";
        let procs = parse_process_list(out);
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].cpu, None);
        assert_eq!(procs[0].mem, None);
        assert_eq!(procs[0].rss_kb, None);
        assert_eq!(procs[0].command, "[kworker/0:1H]");
    }

    /// 命令为空（`ps` 偶有此情形）不得让整行丢失：pid 仍是有用信息。
    #[test]
    fn empty_command_keeps_the_row() {
        // 7 个字段之后只有空白
        let out = "   99        1 root                              0.0  0.0   100 S    \n";
        let procs = parse_process_list(out);
        assert_eq!(procs.len(), 1, "命令为空不该丢行，实得 {procs:?}");
        assert_eq!(procs[0].pid, 99);
        assert_eq!(procs[0].command, "");
    }

    /// 字段不足 7 段的残行丢弃，不 panic（远端输出被截断时的形状）。
    #[test]
    fn truncated_line_is_dropped_not_panicking() {
        for out in ["123", "123 1", "123 1 root 0.0", "  \t \n"] {
            assert!(parse_process_list(out).is_empty(), "残行须被丢弃：{out:?}");
        }
    }

    /// 信号白名单：认名字、认 SIG 前缀、认编号；未知值返回 None 而**不回落 TERM**。
    #[test]
    fn signal_parsing_is_closed_and_does_not_default() {
        for (s, want) in [
            ("TERM", Signal::Term),
            ("sigterm", Signal::Term),
            ("15", Signal::Term),
            ("HUP", Signal::Hup),
            ("1", Signal::Hup),
            ("INT", Signal::Int),
            ("KILL", Signal::Kill),
            ("  kill  ", Signal::Kill),
        ] {
            assert_eq!(Signal::parse(s), Some(want), "{s:?} 应解析为 {want:?}");
        }
        for s in ["STOP", "SIGSTOP", "9;rm -rf /", "", "0", "TERM TERM"] {
            assert_eq!(Signal::parse(s), None, "{s:?} 不该被接受");
        }
    }

    /// kill 命令里不存在用户可控的自由文本（注入面为零）。
    #[test]
    fn kill_command_has_no_injection_surface() {
        assert_eq!(kill_command(4321, "TERM").unwrap(), "kill -s TERM 4321");
        assert_eq!(kill_command(1, "HUP").unwrap(), "kill -s HUP 1");
        // 带元字符的信号名被白名单挡在生成之前
        let e = kill_command(10, "TERM; rm -rf /").unwrap_err();
        assert_eq!(e, KillReject::UnknownSignal("TERM; rm -rf /".into()));
        // 生成出来的命令里只可能有 [a-z-] 与十进制数字
        let cmd = kill_command(u32::MAX, "KILL").unwrap();
        assert!(
            cmd.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-'),
            "命令串出现了意外字符：{cmd:?}"
        );
    }

    /// PID 0 拒绝（POSIX 里是「整个进程组」）；PID 1 **不**拒绝，只要求重话确认。
    #[test]
    fn pid_zero_rejected_pid_one_allowed_but_loud() {
        assert_eq!(kill_command(0, "TERM").unwrap_err(), KillReject::PidZero);
        assert!(
            kill_command(1, "TERM").is_ok(),
            "容器里 1 号常是要重启的服务，不该一刀切禁掉"
        );
        assert!(needs_loud_confirm(1, Signal::Term), "PID 1 须重话确认");
        assert!(needs_loud_confirm(999, Signal::Kill), "KILL 须重话确认");
        assert!(!needs_loud_confirm(999, Signal::Term), "普通 TERM 无需重话");
        assert!(!needs_loud_confirm(999, Signal::Hup));
    }

    /// 空列表要能区分「失败」与「真的没解析出」——两者都不能显示成空表。
    #[test]
    fn empty_result_is_classified_not_shown_as_no_processes() {
        // 有进程 → 不是空态
        let ok = "    1        0 root  0.0  0.1  100 Ss   /sbin/init\n";
        assert_eq!(classify_empty(Some(0), ok, ""), None);
        // 非零退出
        let m = classify_empty(Some(127), "", "ps: not found").expect("须判为失败");
        assert!(m.contains("not found"), "实得 {m}");
        assert!(m.contains("127"), "须带上 exit code，实得 {m}");
        // 退出码 0 但无可解析行
        let m2 = classify_empty(Some(0), "\n\n", "").expect("须判为空态");
        assert!(m2.contains("ps"), "实得 {m2}");
        // 有 stderr 但退出码 0（部分实现如此）：仍算失败并带出原因
        let m3 = classify_empty(Some(0), "", "Permission denied").expect("须判为失败");
        assert!(m3.contains("Permission denied"), "实得 {m3}");
        // 未观测到退出码（超时/通道异常）也是失败，且必须说出来——把它当 0 会显示成空表
        let m4 = classify_empty(None, "", "").expect("None 也须判为失败");
        assert!(m4.contains("未观测到"), "实得 {m4}");
    }

    /// 采集命令的形状约束：字段名后带 `=`（抑制表头）、`args` 收尾、只读。
    #[test]
    fn list_command_shape() {
        let c = process_list_command();
        assert!(c.contains("pid="), "字段须带 = 抑制表头");
        assert!(c.contains("user:32="), "用户名须给足宽度，否则被截断");
        assert!(
            c.find("args=").unwrap() > c.find("stat=").unwrap(),
            "含空格的 args 必须排在最后"
        );
        // 只读：不得出现任何改状态的动词，也不得有任何重定向。
        // `>` 一并禁掉是有具体理由的：曾在这条命令上挂过 `2>/dev/null`，而它掐掉的
        // 正是 classify_empty 判「远端不支持」所依赖的 stderr——静默失败会显示成
        // 一张空进程表，读起来像「这台机器上没有进程」。
        for bad in ["kill", "rm ", ">", "mv ", "chmod", "tee"] {
            assert!(
                !c.contains(bad),
                "采集命令必须只读且不重定向，出现了 {bad:?}"
            );
        }
    }
}
