//! 从终端回滚里提取候选命令（M4a 历史命令 UI 的第二个数据源）。
//!
//! ## 这是启发式，不是解析
//!
//! 屏幕上没有任何标记能区分「提示符后面用户敲的那一行」与「命令输出里恰好长得
//! 像提示符的一行」——shell 不把这个信息发给终端。所以本模块**只能**猜，而且一定
//! 有猜错的时候（`echo "server$ down"` 的输出会被当成一条命令）。
//!
//! 既然错是必然的，工程上要做的三件事是：
//! ① **保守**：宁可漏提取，不要多提取。多出来的假命令会被用户一键重发，那是真事故。
//! ② **可标注**：提取结果一律记为 `source = "grid"`，UI 明示「从屏幕提取，可能不准」，
//!    与本程序亲手发出的字节（`sent`，字节精确）分开。
//! ③ **不假装准确**：本模块不做「智能」还原（不猜续行、不拼多行命令）。
//!
//! ## 提示符的判据
//!
//! 行首起、到**第一个** `$`/`#`/`%`/`>` 且其后紧跟空白为止，视为提示符；其后是候选
//! 命令。选「第一个」而不是「最后一个」：`git commit -m "fix: a > b"` 里的 `>` 在
//! 命令中间，按最后一个切会把命令劈成两半、只留 `b"`。

/// 提示符前缀的长度上限。
///
/// 真实提示符再花哨也不过百来字符（用户名@主机:长路径）。超过这个长度还没遇到
/// 提示符字符的行，几乎必然是输出行（日志、表格、堆栈），按提示符处理只会造假。
const MAX_PROMPT_LEN: usize = 200;

/// 候选命令的长度上限。超长的「命令」基本都是被误切的输出行（如一行很长的 JSON）。
const MAX_COMMAND_LEN: usize = 2000;

/// 从一行里提取候选命令。不像提示符行则返回 `None`。
pub fn extract_from_line(line: &str) -> Option<String> {
    let line = line.trim_end();
    if line.is_empty() {
        return None;
    }
    let bytes = line.as_bytes();
    let scan = bytes.len().min(MAX_PROMPT_LEN);
    for i in 0..scan {
        if !matches!(bytes[i], b'$' | b'#' | b'%' | b'>') {
            continue;
        }
        // 提示符字符后必须紧跟空白，否则命中的是命令里的 `$VAR`、`#comment`、
        // `2>&1` 这类东西
        let rest = &line[i + 1..];
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let cmd = rest.trim();
        if cmd.is_empty() || cmd.len() > MAX_COMMAND_LEN {
            return None;
        }
        // 提取出来的东西自己又以提示符字符结尾（`... $`），说明切错了位置
        if cmd.ends_with(['$', '#', '%']) {
            return None;
        }
        return Some(cmd.to_string());
    }
    None
}

/// 从整段回滚文本里提取候选命令，按出现顺序去重（保留最先出现的那次）。
///
/// 去重在这里做而不是留给数据库：同一屏回滚里同一条命令出现几十次是常态
/// （一个循环里反复敲 `ls`），逐条走一次数据库写入纯属浪费。
pub fn extract_commands(scrollback: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in scrollback.lines() {
        if let Some(cmd) = extract_from_line(line) {
            if seen.insert(cmd.clone()) {
                out.push(cmd);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 常见提示符形态都能提出命令。
    #[test]
    fn extracts_from_common_prompt_shapes() {
        let cases = [
            (
                "alice@web-01:~/src$ cargo build --release",
                "cargo build --release",
            ),
            ("[root@db1 /var/log]# tail -f syslog", "tail -f syslog"),
            ("user% brew install jq", "brew install jq"),
            ("PS> Get-Process", "Get-Process"),
            ("$ ls", "ls"),
            ("# whoami", "whoami"),
            // 提示符与命令之间多个空格
            ("me@host:~$    df -h", "df -h"),
        ];
        for (line, want) in cases {
            assert_eq!(
                extract_from_line(line).as_deref(),
                Some(want),
                "输入 {line:?}"
            );
        }
    }

    /// 命令里的 `$`/`#`/`>` 不得被当成提示符——切错了会把命令劈成两半。
    #[test]
    fn does_not_split_on_prompt_chars_inside_the_command() {
        // `$VAR` 后面不是空白，跳过；真正的提示符在前面
        assert_eq!(
            extract_from_line("me@h:~$ echo $HOME").as_deref(),
            Some("echo $HOME")
        );
        // 重定向 `2>&1`：`>` 后紧跟 `&`，不是空白
        assert_eq!(
            extract_from_line("me@h:~$ cmd 2>&1").as_deref(),
            Some("cmd 2>&1")
        );
        // 注释里的 `#`
        assert_eq!(
            extract_from_line("me@h:~$ ls # 列目录").as_deref(),
            Some("ls # 列目录")
        );
        // 取**第一个**提示符字符：取最后一个会把这条命令劈成 `b\"`
        assert_eq!(
            extract_from_line(r#"me@h:~$ git commit -m "fix: a > b""#).as_deref(),
            Some(r#"git commit -m "fix: a > b""#)
        );
    }

    /// 输出行里的 `$VAR` / `#tag` / `2>&1` 不得被当成提示符。
    ///
    /// 这一条才是「提示符字符后必须紧跟空白」这条规则的**存在理由**。上面那条
    /// `does_not_split_on_prompt_chars_inside_the_command` 里的样例，真提示符都排在
    /// 那些字符**前面**，于是把该规则整条删掉也照过（变异验证实测）。这里的样例
    /// 让这些字符出现在**没有提示符**的行里，删掉规则就会凭空造出命令。
    #[test]
    fn output_lines_containing_prompt_chars_yield_nothing() {
        let cases = [
            "export PATH=$HOME/bin",          // 删了规则会提出 "HOME/bin"
            "Running cmd 2>&1 in background", // 会提出 "&1 in background"
            "See https://docs/install#linux", // 会提出 "linux"
            "Loaded 100%complete",            // 会提出 "complete"
            "value=$x",                       // 会提出 "x"
        ];
        for line in cases {
            assert_eq!(
                extract_from_line(line),
                None,
                "输出行不该被提取成命令：{line:?}"
            );
        }
    }

    /// 不像提示符行的一律不提取（宁可漏，不要造假命令让人一键重发）。
    #[test]
    fn rejects_non_prompt_lines() {
        for line in [
            "",
            "   ",
            "total 48",                      // ls 的输出
            "drwxr-xr-x 2 root root 4096 .", // 无提示符字符
            "me@host:~$",                    // 只有提示符、没有命令
            "me@host:~$ ",                   // 同上（尾随空格）
            "Progress: 45% done",            // `%` 后是空白但整行是输出——见下方说明
        ] {
            let got = extract_from_line(line);
            if line.contains('%') {
                // 如实记录：这一条**会**被误提取成 "done"。启发式的固有代价，
                // 所以结果一律标 source=grid 并在 UI 明示「可能不准」。
                assert_eq!(got.as_deref(), Some("done"), "已知误判形态：{line:?}");
            } else {
                assert_eq!(got, None, "不该提取：{line:?}");
            }
        }
    }

    /// 超长的提示符前缀（其实是输出行）不提取。
    #[test]
    fn rejects_overlong_prompt_prefix() {
        let long = format!("{}$ cmd", "x".repeat(MAX_PROMPT_LEN + 10));
        assert_eq!(extract_from_line(&long), None, "提示符前缀超长即视为输出行");
        // 刚好在界内仍提取
        let ok = format!("{}$ cmd", "x".repeat(MAX_PROMPT_LEN - 5));
        assert_eq!(extract_from_line(&ok).as_deref(), Some("cmd"));
    }

    /// 超长的候选命令不提取（多半是被误切的一行长 JSON）。
    #[test]
    fn rejects_overlong_command() {
        let line = format!("h$ {}", "a".repeat(MAX_COMMAND_LEN + 1));
        assert_eq!(extract_from_line(&line), None);
    }

    /// 以提示符字符收尾的提取结果说明切错了位置。
    #[test]
    fn rejects_result_ending_in_prompt_char() {
        assert_eq!(extract_from_line("foo > bar $"), None);
        assert_eq!(extract_from_line("a # b #"), None);
    }

    /// 整段提取：按出现顺序去重，输出行被跳过。
    #[test]
    fn extracts_and_dedupes_in_order() {
        let text = "\
alice@web:~$ ls
total 8
drwxr-xr-x 2 alice alice 4096 Aug 21 10:00 src
alice@web:~$ cargo test
   Compiling foo v0.1.0
test result: ok. 3 passed
alice@web:~$ ls
alice@web:~$ git status
";
        assert_eq!(
            extract_commands(text),
            vec!["ls", "cargo test", "git status"],
            "须按出现顺序去重"
        );
    }

    /// 空输入与纯输出不产出任何候选。
    #[test]
    fn no_prompts_yields_nothing() {
        assert!(extract_commands("").is_empty());
        assert!(extract_commands("line one\nline two\n").is_empty());
    }

    /// 多字节字符不会切在字符中间（`line[i+1..]` 的切片边界）。
    #[test]
    fn multibyte_lines_do_not_panic() {
        // 中文提示符路径 + 中文命令参数
        assert_eq!(
            extract_from_line("我@主机:~/项目$ grep 错误 日志.txt").as_deref(),
            Some("grep 错误 日志.txt")
        );
        // 提示符字符前后都是多字节
        for line in ["中文中文中文", "路径→目录", "○●◎", "→$→"] {
            let _ = extract_from_line(line); // 不 panic 即可
        }
    }
}
