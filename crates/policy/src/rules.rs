//! 分级规则（总设计 §4.3）。
//!
//! # 判定顺序（§4.3 硬约束 4）
//!
//! 危险谓词 → 白名单。**顺序不能反**：反过来就会出现「第一个组分白名单命中即放行」，
//! 而 `ls; rm -rf ~` 的第一个组分正是 `ls`。
//!
//! 具体流程：
//!
//! 1. 解析失败 → `Dangerous`（M2 出口原文）。
//! 2. 逐条**管道**跑管道级谓词（需要跨阶段关系的规则，如「网络内容流进解释器」）。
//! 3. 逐条**简单命令**跑命令级谓词（先剥包装器：`sudo`/`env`/`timeout`/`xargs`…）。
//! 4. 脚本级构造抬底（命令替换 / 定不了值的展开 / 进程替换 / 子 shell / 函数定义
//!    一律 ≥ `Write`，§4.3 硬约束 2、3）。
//! 5. 白名单：**所有**组分各自单独命中、且第 2–4 步没有任何抬级理由时，整体 `ReadOnly`。
//! 6. 兜底 `Write`。
//!
//! # 每条规则都带稳定 id
//!
//! [`Reason::rule`] 是稳定串，测试与审计都按它引用。加规则可以，**改已有 id 不行**
//! ——已落库的审计行按这个串比对，改名等于让历史记录失去含义。

use crate::paths::{classify_path, PathClass};
use crate::shell::{self, ParseError, Pipeline, Script, SimpleCommand, Word};
use crate::Tier;

/// 一条判定理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    /// 稳定规则 id（**永不改名**）。
    pub rule: &'static str,
    /// 这条规则贡献的级别。
    pub tier: Tier,
    /// 人话说明，直接进 UI 确认框与审计行。
    pub detail: String,
}

/// 分级结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub tier: Tier,
    /// 所有命中的理由，按发现顺序。空表示「无任何可疑之处」。
    pub reasons: Vec<Reason>,
}

impl Verdict {
    fn one(rule: &'static str, tier: Tier, detail: impl Into<String>) -> Self {
        Self {
            tier,
            reasons: vec![Reason {
                rule,
                tier,
                detail: detail.into(),
            }],
        }
    }

    /// 最严的那条理由（UI 只展示一条时用它）。
    pub fn primary(&self) -> Option<&Reason> {
        self.reasons.iter().max_by_key(|r| r.tier)
    }
}

// ── 规则 id（稳定串，永不改名）────────────────────────────────────────────
pub const RULE_PARSE_ERROR: &str = "parse_error";
pub const RULE_EMPTY: &str = "empty";
pub const RULE_FETCH_TO_INTERPRETER: &str = "fetch_piped_to_interpreter";
pub const RULE_DECODE_TO_INTERPRETER: &str = "decode_piped_to_interpreter";
pub const RULE_PIPE_TO_INTERPRETER: &str = "pipe_to_interpreter";
pub const RULE_EVAL: &str = "eval_like";
pub const RULE_OPAQUE_INTERPRETER: &str = "opaque_interpreter_inline_code";
pub const RULE_SHELL_INLINE: &str = "shell_inline_code";
pub const RULE_DESTRUCTIVE_PATH: &str = "destructive_command_on_sensitive_path";
pub const RULE_ROOTISH_RECURSIVE: &str = "recursive_delete_at_root_or_home";
pub const RULE_BLOCK_DEVICE: &str = "writes_block_device";
pub const RULE_WRITE_REDIRECT_SENSITIVE: &str = "write_redirect_to_sensitive_path";
pub const RULE_WRITE_REDIRECT: &str = "write_redirect";
pub const RULE_COPY_TO_SENSITIVE: &str = "copy_or_move_to_sensitive_path";
pub const RULE_FORK_BOMB: &str = "fork_bomb";
pub const RULE_PERSISTENCE: &str = "persistence_install";
pub const RULE_REVERSE_SHELL: &str = "reverse_shell";
pub const RULE_WINDOWS_DESTRUCTIVE: &str = "windows_destructive";
pub const RULE_UNKNOWN_PROGRAM_SENSITIVE_ARG: &str = "unknown_program_with_sensitive_arg";
pub const RULE_COMMAND_SUBSTITUTION: &str = "command_substitution";
pub const RULE_UNRESOLVED_EXPANSION: &str = "unresolved_expansion";
pub const RULE_PROCESS_SUBSTITUTION: &str = "process_substitution";
pub const RULE_SUBSHELL: &str = "subshell_or_group";
pub const RULE_FUNCTION_DEF: &str = "function_definition";
pub const RULE_HEREDOC: &str = "heredoc";
pub const RULE_NON_PLAIN_WORD: &str = "non_literal_word";
pub const RULE_PRIVILEGE: &str = "privilege_escalation";
pub const RULE_NOT_WHITELISTED: &str = "not_whitelisted";

/// 分级入口。
///
/// 纯函数：同样的输入永远得到同样的结论。不读环境变量、不碰文件系统——
/// 否则同一条命令在两台机器上分级不同，既无法测试也无法向用户解释。
pub fn classify(input: &str) -> Verdict {
    let script = match shell::parse(input) {
        Ok(s) => s,
        Err(e) => {
            return Verdict::one(
                RULE_PARSE_ERROR,
                Tier::Dangerous,
                format!("无法可靠解析这条命令（{e}）。看不懂的东西一律按最危险处理"),
            )
        }
    };
    classify_script(&script, 0)
}

/// 递归深度上限：`bash -c 'bash -c "…"'`。与解析器的深度闸同理——
/// 没有上限就是一条由输入控制的栈深。
const MAX_RECURSION: usize = 4;

fn classify_script(script: &Script, recursion: usize) -> Verdict {
    let mut reasons: Vec<Reason> = Vec::new();

    if script.commands().next().is_none() {
        // 空输入 / 纯注释：没有要执行的东西。不是「安全」，是「无事发生」。
        return Verdict {
            tier: Tier::ReadOnly,
            reasons: vec![],
        };
    }

    // ── 管道级 ──
    for p in &script.pipelines {
        pipeline_rules(p, &mut reasons);
    }

    // ── 命令级 ──
    //
    // **包装器只剥一次，规则与白名单共用同一个结果。** 分两次剥（或者规则剥了、
    // 白名单没剥）会得出自相矛盾的结论：`env ls` 的危险谓词看的是 `ls`，
    // 而白名单看的是 `env`——后者不在表里，于是一条纯只读命令被判成 write。
    let peeled: Vec<SimpleCommand> = script
        .commands()
        .map(|c| {
            let mut rs = Vec::new();
            let p = peel(c, &mut rs);
            reasons.extend(rs);
            p
        })
        .collect();
    for c in &peeled {
        command_rules(c, script, &mut reasons, recursion);
    }

    // ── 脚本级构造抬底（§4.3 硬约束 2、3）──
    let t = &script.traits;
    if t.command_substitution {
        reasons.push(Reason {
            rule: RULE_COMMAND_SUBSTITUTION,
            tier: Tier::Write,
            detail: "含命令替换：真正执行的内容由另一条命令的输出决定，无法静态判定为只读".into(),
        });
    }
    if t.unresolved_expansion {
        reasons.push(Reason {
            rule: RULE_UNRESOLVED_EXPANSION,
            tier: Tier::Write,
            detail: "含取值无法确定的变量展开：展开后可能是任何命令".into(),
        });
    }
    if t.process_substitution {
        reasons.push(Reason {
            rule: RULE_PROCESS_SUBSTITUTION,
            tier: Tier::Write,
            detail: "含进程替换：括号内的命令会被执行".into(),
        });
    }
    if t.subshell {
        reasons.push(Reason {
            rule: RULE_SUBSHELL,
            tier: Tier::Write,
            detail: "含子 shell / 命令组".into(),
        });
    }
    if t.function_definition {
        reasons.push(Reason {
            rule: RULE_FUNCTION_DEF,
            tier: Tier::Write,
            detail: "定义了函数".into(),
        });
    }
    if t.heredoc {
        reasons.push(Reason {
            rule: RULE_HEREDOC,
            tier: Tier::Write,
            detail: "含 heredoc：有数据被写入某处".into(),
        });
    }

    // ── 白名单（§4.3 硬约束 4：仅当所有组分各自单独命中）──
    let elevated = reasons
        .iter()
        .map(|r| r.tier)
        .max()
        .unwrap_or(Tier::ReadOnly);
    if elevated == Tier::ReadOnly {
        let mut all_ro = true;
        for c in &peeled {
            if !is_read_only(c) {
                all_ro = false;
                reasons.push(Reason {
                    rule: RULE_NOT_WHITELISTED,
                    tier: Tier::Write,
                    detail: format!(
                        "`{}` 不在只读白名单里",
                        c.program_basename().unwrap_or("<赋值语句>")
                    ),
                });
            }
        }
        if all_ro {
            return Verdict {
                tier: Tier::ReadOnly,
                reasons,
            };
        }
    }

    // 兜底 Write（§4.3「非只读且非危险」）
    let tier = reasons
        .iter()
        .map(|r| r.tier)
        .max()
        .unwrap_or(Tier::Write)
        .max(Tier::Write);
    Verdict { tier, reasons }
}

// ══════════════════════════════════════════════════════════════════════
// 管道级谓词
// ══════════════════════════════════════════════════════════════════════

/// 会把**远端内容**取到本地的程序。
const FETCHERS: &[&str] = &[
    "curl",
    "wget",
    "fetch",
    "aria2c",
    "axel",
    "nc",
    "ncat",
    "netcat",
    "socat",
    "ftp",
    "tftp",
    "lynx",
    "links",
    "w3m",
    "httpie",
    "http",
    "iwr",
    "Invoke-WebRequest",
];

/// 会把输入**当代码执行**的程序。
const INTERPRETERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "csh",
    "tcsh",
    "ash",
    "busybox",
    "python",
    "python2",
    "python3",
    "perl",
    "ruby",
    "node",
    "nodejs",
    "php",
    "lua",
    "Rscript",
    "osascript",
    "powershell",
    "pwsh",
    "cmd",
    "wscript",
    "cscript",
    "deno",
    "bun",
];

/// 会把「看不出内容的东西」变成可执行文本的程序。
const DECODERS: &[&str] = &[
    "base64", "base32", "xxd", "uudecode", "uuencode", "openssl", "gunzip", "zcat", "bunzip2",
    "xzcat", "unxz", "zstdcat", "certutil",
];

fn pipeline_rules(p: &Pipeline, out: &mut Vec<Reason>) {
    if p.stages.len() < 2 {
        return;
    }
    // 找出「解释器」阶段的最早位置——只有位于它**之前**的阶段才算「喂给它」
    let interp_at = p
        .stages
        .iter()
        .position(|s| s.program_basename().is_some_and(is_interpreter));
    let Some(interp_at) = interp_at else {
        return;
    };
    let upstream = &p.stages[..interp_at];
    let interp = p.stages[interp_at].program_basename().unwrap_or("?");

    if upstream
        .iter()
        .any(|s| s.program_basename().is_some_and(|b| FETCHERS.contains(&b)))
    {
        out.push(Reason {
            rule: RULE_FETCH_TO_INTERPRETER,
            tier: Tier::Dangerous,
            detail: format!(
                "从网络取到的内容被直接交给 `{interp}` 执行：远端改一个字节就是任意代码执行"
            ),
        });
        return;
    }
    if upstream
        .iter()
        .any(|s| s.program_basename().is_some_and(|b| DECODERS.contains(&b)))
    {
        out.push(Reason {
            rule: RULE_DECODE_TO_INTERPRETER,
            tier: Tier::Dangerous,
            detail: format!("解码后的内容被直接交给 `{interp}` 执行：命令的真实内容不在这行文本里"),
        });
        return;
    }
    // §4.3 硬约束 3 的 `* | sh|bash`：不知道来源，但确实在执行别处的内容
    out.push(Reason {
        rule: RULE_PIPE_TO_INTERPRETER,
        tier: Tier::Write,
        detail: format!("有内容被管道交给 `{interp}` 执行"),
    });
}

fn is_interpreter(b: &str) -> bool {
    INTERPRETERS.contains(&b) || INTERPRETERS.contains(&b.trim_end_matches(".exe"))
}

// ══════════════════════════════════════════════════════════════════════
// 包装器剥离
// ══════════════════════════════════════════════════════════════════════

/// 提权类包装器：本身就不该是 read_only。
const PRIVILEGE_WRAPPERS: &[&str] = &["sudo", "doas", "pkexec", "runuser", "su"];

/// 透明包装器：只改变执行环境，不改变「跑什么」。
const TRANSPARENT_WRAPPERS: &[&str] = &[
    "env", "nohup", "setsid", "nice", "ionice", "stdbuf", "time", "timeout", "chrt", "taskset",
    "unbuffer", "flock", "watch", "xargs", "exec", "command", "builtin",
];

/// 这些包装器的旗标会**吃掉下一个词**。
fn flag_takes_value(wrapper: &str, flag: &str) -> bool {
    match wrapper {
        "sudo" | "doas" => matches!(flag, "-u" | "-g" | "-p" | "-C" | "-h" | "-U" | "-r" | "-t"),
        "su" => matches!(flag, "-c" | "-s" | "-g" | "-G" | "--command" | "--shell"),
        "env" => matches!(flag, "-u" | "--unset" | "-C" | "--chdir" | "-S"),
        "timeout" => matches!(flag, "-s" | "--signal" | "-k" | "--kill-after"),
        "nice" | "ionice" => matches!(flag, "-n" | "-c" | "-p"),
        "stdbuf" => matches!(flag, "-i" | "-o" | "-e"),
        "xargs" => matches!(
            flag,
            "-n" | "-L" | "-P" | "-I" | "-i" | "-d" | "-s" | "-E" | "-a"
        ),
        "flock" => matches!(flag, "-w" | "--wait" | "-E"),
        "watch" => matches!(flag, "-n" | "-d"),
        _ => false,
    }
}

/// 剥掉一层包装器，返回（内层命令，这层包装器带来的底线级别）。
///
/// 剥离是必须的：`sudo rm -rf /` 若只看首词 `sudo`，它不在白名单里所以落到 `Write`
/// ——而它显然是 `Dangerous`。同理 `timeout 5 rm -rf /`、`xargs rm -rf /`、
/// `env A=1 rm -rf /`。
fn unwrap_once(cmd: &SimpleCommand) -> Option<(SimpleCommand, Option<Reason>)> {
    let w = cmd.program_basename()?;
    let is_priv = PRIVILEGE_WRAPPERS.contains(&w);
    if !is_priv && !TRANSPARENT_WRAPPERS.contains(&w) {
        return None;
    }
    // `su -c '…'` 的代码在旗标的**值**里，不是后面的位置参数——剥不出内层命令。
    // 把它留给 inline_code_rules（`su` 已在那边的 shell 列表里），
    // 否则这里会把 `-c` 的值当成程序名，而那个「程序名」是一整段 shell 代码。
    if w == "su"
        && cmd
            .args()
            .iter()
            .any(|a| matches!(a.text.as_str(), "-c" | "--command"))
    {
        return None;
    }
    let args = cmd.args();
    let mut i = 0usize;
    // 跳旗标（及其取值）、赋值（`env A=1`）、纯数字（`timeout 5`）
    while i < args.len() {
        let t = args[i].text.as_str();
        if t == "--" {
            i += 1;
            break;
        }
        if t.starts_with('-') && t.len() > 1 {
            if flag_takes_value(w, t) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if w == "env" && t.contains('=') {
            i += 1;
            continue;
        }
        if (w == "timeout" || w == "nice" || w == "ionice")
            && t.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            i += 1;
            continue;
        }
        break;
    }
    let rest = args.get(i..).unwrap_or(&[]);
    let reason = is_priv.then(|| Reason {
        rule: RULE_PRIVILEGE,
        tier: Tier::Write,
        detail: format!("经 `{w}` 提权执行：无论内层是什么，提权本身就不能自动放行"),
    });
    if rest.is_empty() {
        // 只有包装器没有内层命令（`sudo -l`、裸 `env`）：没什么可剥的
        return reason.map(|r| (SimpleCommand::default(), Some(r)));
    }
    let inner = SimpleCommand {
        assignments: Vec::new(),
        argv: rest.to_vec(),
        // 重定向与 heredoc 属于整条简单命令，剥包装器时要带过去
        // （`sudo bash <<EOF … EOF` 的正文仍然是喂给 bash 的代码）
        redirects: cmd.redirects.clone(),
        heredoc_bodies: cmd.heredoc_bodies.clone(),
        depth: cmd.depth,
    };
    Some((inner, reason))
}

/// 把所有包装器剥到底。
fn peel(cmd: &SimpleCommand, out: &mut Vec<Reason>) -> SimpleCommand {
    let mut cur = cmd.clone();
    for _ in 0..8 {
        match unwrap_once(&cur) {
            Some((inner, reason)) => {
                if let Some(r) = reason {
                    out.push(r);
                }
                if inner.argv.is_empty() {
                    return cur;
                }
                cur = inner;
            }
            None => break,
        }
    }
    cur
}

// ══════════════════════════════════════════════════════════════════════
// 命令级谓词
// ══════════════════════════════════════════════════════════════════════

/// 会删除/覆盖文件的程序（判定其路径参数）。
const DESTRUCTIVE: &[&str] = &[
    "rm",
    "rmdir",
    "unlink",
    "shred",
    "truncate",
    "mv",
    "cp",
    "install",
    "ln",
    "rsync",
    "tee",
    "dd",
    "chmod",
    "chown",
    "chgrp",
    "chattr",
    "setfacl",
    "mkfs",
    "wipefs",
    "blkdiscard",
    "sgdisk",
    "fdisk",
    "parted",
    "mkswap",
    "sed",
    "perl",
    "python",
    "tar",
    "unzip",
];

/// 判定一条简单命令。
fn command_rules(cmd: &SimpleCommand, script: &Script, out: &mut Vec<Reason>, recursion: usize) {
    // 没有程序名的命令仍可能有副作用：`> /etc/passwd` 就是一条 argv 为空、
    // 只有重定向的命令，它会把那个文件截成 0 字节。
    // 第一版在这里直接 return，于是这条命令一条理由都没有、落到兜底 Write。
    if cmd.argv.is_empty() {
        redirect_rules(cmd, "<重定向>", out);
        return;
    }
    let Some(prog) = cmd.program_basename() else {
        return;
    };
    let prog_lower = prog.to_ascii_lowercase();
    let prog_l = prog_lower.trim_end_matches(".exe");

    // 词不是普通字面量 → 永不 read_only（§4.3 硬约束 2）
    if !cmd.all_words_plain() {
        out.push(Reason {
            rule: RULE_NON_PLAIN_WORD,
            tier: Tier::Write,
            detail: format!("`{prog}` 的参数里有引号/转义/展开，最终值不由这段文本单独决定"),
        });
    }

    // ── 写重定向 ──
    redirect_rules(cmd, prog, out);

    // ── eval / source ──
    if matches!(prog_l, "eval" | "source" | ".") {
        let any_opaque = cmd.args().iter().any(|w| !w.traits.is_plain());
        out.push(Reason {
            rule: RULE_EVAL,
            tier: if any_opaque {
                Tier::Dangerous
            } else {
                Tier::Write
            },
            detail: if any_opaque {
                format!("`{prog}` 会把一段**动态生成**的文本当命令执行")
            } else {
                format!("`{prog}` 会把参数当命令执行")
            },
        });
    }

    // ── 解释器内联代码 ──
    inline_code_rules(cmd, prog_l, out, recursion);

    // ── 破坏性程序的路径参数 ──
    if DESTRUCTIVE.contains(&prog_l) || prog_l.starts_with("mkfs") {
        destructive_path_rules(cmd, prog_l, out);
    }

    // ── find 的动作旗标 ──
    if matches!(prog_l, "find" | "fd") {
        find_rules(cmd, out, recursion);
    }

    // ── 喂给解释器的 heredoc 正文是**代码**，不是数据 ──
    if !cmd.heredoc_bodies.is_empty() && is_interpreter(prog_l) {
        heredoc_code_rules(cmd, prog_l, out, recursion);
    }

    // ── 持久化安装 ──
    if prog_l == "crontab" {
        let installs = cmd
            .args()
            .iter()
            .any(|w| w.text == "-" || w.text == "-r" || !w.text.starts_with('-'));
        if installs {
            out.push(Reason {
                rule: RULE_PERSISTENCE,
                tier: Tier::Dangerous,
                detail: "改写 crontab：装进去的东西会被系统反复执行".into(),
            });
        }
    }

    // ── 反弹 shell ──
    if matches!(prog_l, "nc" | "ncat" | "netcat" | "socat") {
        let execs = cmd.args().iter().any(|w| {
            let t = w.text.as_str();
            t == "-e"
                || t == "-c"
                || t == "--exec"
                || t.starts_with("EXEC:")
                || t.starts_with("SYSTEM:")
        });
        if execs {
            out.push(Reason {
                rule: RULE_REVERSE_SHELL,
                tier: Tier::Dangerous,
                detail: format!("`{prog}` 被要求把网络连接接到一个程序上：这是反弹 shell 的形状"),
            });
        }
    }

    // ── curl/wget 的落地路径 ──
    if matches!(prog_l, "curl" | "wget") {
        for (i, a) in cmd.args().iter().enumerate() {
            let is_out_flag = matches!(
                a.text.as_str(),
                "-o" | "-O" | "--output" | "--output-document"
            );
            // 两种写法：`-o PATH`（值在下一个词）与 `--output=PATH`（值粘在一起）
            let target: Option<&str> = if is_out_flag {
                cmd.args().get(i + 1).map(|w| w.text.as_str())
            } else {
                a.text.strip_prefix("--output=")
            };
            let Some(target) = target else { continue };
            let class = classify_path(target);
            if class != PathClass::Ordinary {
                out.push(Reason {
                    rule: RULE_DESTRUCTIVE_PATH,
                    tier: Tier::Dangerous,
                    detail: sensitive_detail(target, class, "从网络下载的内容被写入"),
                });
            }
        }
    }

    // ── Windows 破坏性命令 ──
    windows_rules(cmd, prog_l, out);

    // ── fork 炸弹 ──
    fork_bomb_rules(cmd, script, out);

    // ── 程序名定不了值，但参数指向敏感位置 ──
    if cmd.argv[0].traits.param_expansion && !cmd.argv[0].traits.is_plain() {
        for a in cmd.args() {
            if classify_path(&a.text) != PathClass::Ordinary || is_rootish(&a.text) {
                out.push(Reason {
                    rule: RULE_UNKNOWN_PROGRAM_SENSITIVE_ARG,
                    tier: Tier::Dangerous,
                    detail: format!(
                        "程序名来自变量（无法确定是什么），而参数 `{}` 指向敏感位置",
                        a.text
                    ),
                });
                break;
            }
        }
    }
}

/// 写重定向的目标判定。抽出来是因为「只有重定向、没有程序名」的命令也要走它。
fn redirect_rules(cmd: &SimpleCommand, prog: &str, out: &mut Vec<Reason>) {
    let _ = prog;
    for r in &cmd.redirects {
        if !r.kind.writes() {
            continue;
        }
        // 丢弃式伪设备不算一次写：`ls > /dev/null` 与 `ls` 对系统的影响完全相同。
        if crate::paths::is_discard_sink(&r.target.text) {
            continue;
        }
        let class = classify_path(&r.target.text);
        if class == PathClass::Ordinary {
            out.push(Reason {
                rule: RULE_WRITE_REDIRECT,
                tier: Tier::Write,
                detail: format!("输出被写入 `{}`", r.target.text),
            });
        } else {
            out.push(Reason {
                rule: RULE_WRITE_REDIRECT_SENSITIVE,
                tier: Tier::Dangerous,
                detail: sensitive_detail(&r.target.text, class, "输出被写入"),
            });
        }
    }
}

/// `find` 的动作旗标：带上它们就不只是「找」了。
const FIND_ACTIONS: &[&str] = &[
    "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fls", "-fprint", "-fprint0", "-fprintf",
];

/// `find … -delete` / `find … -exec CMD` 的判定。
///
/// 需要单独一条是因为 `find` 本身不在破坏性程序表里（它通常只是找东西），
/// 而 `find / -delete` 会删掉整棵树、`find . -exec rm -rf {} \;` 借 find 之手跑 rm。
/// 只看首词 `find` 的引擎对这两条一无所知。
fn find_rules(cmd: &SimpleCommand, out: &mut Vec<Reason>, recursion: usize) {
    let args = cmd.args();
    let Some(at) = args
        .iter()
        .position(|a| FIND_ACTIONS.contains(&a.text.as_str()))
    else {
        return;
    };
    let action = args[at].text.as_str();
    // 起点路径：动作旗标之前、不以 `-` 开头的参数
    let roots: Vec<&Word> = args[..at]
        .iter()
        .filter(|a| !a.text.starts_with('-'))
        .collect();
    let root_sensitive = roots
        .iter()
        .any(|r| is_rootish(&r.text) || classify_path(&r.text) != PathClass::Ordinary);

    if action == "-delete" {
        out.push(Reason {
            rule: if root_sensitive {
                RULE_DESTRUCTIVE_PATH
            } else {
                RULE_WRITE_REDIRECT
            },
            tier: if root_sensitive {
                Tier::Dangerous
            } else {
                Tier::Write
            },
            detail: format!(
                "`find … -delete` 会删掉所有匹配项{}",
                if root_sensitive {
                    "，而搜索起点是敏感位置"
                } else {
                    ""
                }
            ),
        });
        return;
    }
    // `-exec CMD …` ：把被执行的那条命令拆出来递归判定
    let inner: Vec<Word> = args[at + 1..]
        .iter()
        .take_while(|a| a.text != ";" && a.text != "+" && a.text != "\\;")
        .cloned()
        .collect();
    if inner.is_empty() || recursion >= MAX_RECURSION {
        out.push(Reason {
            rule: RULE_DESTRUCTIVE_PATH,
            tier: Tier::Dangerous,
            detail: format!("`find … {action}` 会对每个匹配项执行一条命令，但那条命令无法审查"),
        });
        return;
    }
    let sub = SimpleCommand {
        argv: inner,
        depth: cmd.depth,
        ..Default::default()
    };
    let mut sub_reasons = Vec::new();
    let sub = peel(&sub, &mut sub_reasons);
    // 递归只跑命令级谓词：`find -exec` 的内层不是一段脚本，就是一条命令
    let empty = Script::default();
    command_rules(&sub, &empty, &mut sub_reasons, recursion + 1);
    let worst = sub_reasons
        .iter()
        .map(|r| r.tier)
        .max()
        .unwrap_or(Tier::Write);
    out.push(Reason {
        rule: RULE_DESTRUCTIVE_PATH,
        tier: worst.max(Tier::Write),
        detail: format!(
            "`find … {action}` 会对每个匹配项执行 `{}`",
            sub.program_basename().unwrap_or("?")
        ),
    });
    out.extend(sub_reasons);
}

fn sensitive_detail(path: &str, class: PathClass, verb: &str) -> String {
    match class {
        PathClass::CodeExecution => {
            format!("{verb} `{path}`——写进这里等于取得代码执行：下次登录/开机时它会被自动运行")
        }
        PathClass::SystemCritical => format!("{verb} `{path}`——这是系统关键位置"),
        PathClass::Ordinary => format!("{verb} `{path}`"),
    }
}

/// 解释器把一段字符串当代码跑。
fn inline_code_rules(cmd: &SimpleCommand, prog_l: &str, out: &mut Vec<Reason>, recursion: usize) {
    // shell 的 `-c`：这段代码我们**能**解析，所以递归判定，结论就是它的结论。
    // 于是 `bash -c 'ls'` 仍然可以是 read_only，而 `bash -c 'rm -rf /'` 是 dangerous。
    if matches!(
        prog_l,
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "ash" | "busybox" | "su"
    ) {
        if let Some(code) = flag_value(cmd, &["-c", "--command"]) {
            if !code.traits.is_plain() || recursion >= MAX_RECURSION {
                out.push(Reason {
                    rule: RULE_OPAQUE_INTERPRETER,
                    tier: Tier::Dangerous,
                    detail: "交给 shell 执行的那段代码本身是动态拼出来的，无法审查".into(),
                });
                return;
            }
            match shell::parse(&code.text) {
                Ok(inner) => {
                    let v = classify_script(&inner, recursion + 1);
                    out.push(Reason {
                        rule: RULE_SHELL_INLINE,
                        tier: v.tier,
                        detail: format!("`-c` 里的代码经递归判定为 {}", v.tier),
                    });
                    out.extend(v.reasons);
                }
                Err(e) => out.push(Reason {
                    rule: RULE_OPAQUE_INTERPRETER,
                    tier: Tier::Dangerous,
                    detail: format!("`-c` 里的代码无法解析（{e}）"),
                }),
            }
            return;
        }
    }
    // 非 shell 解释器：我们**不会**解析 Python/Perl/Ruby/JS，
    // 所以「这段代码干什么」是完全不可知的。失败闭合 → dangerous。
    //
    // 这是一个刻意的、有代价的取舍：`python -c 'print(1)'` 也会被要求强确认。
    // 反过来的代价是 `python -c 'import os;os.system("rm -rf /")'` 只算 write，
    // 而那正是 §8.1 点名要覆盖的「解释器内联」绕过类目。宁可多问一次。
    let code_flags: &[&str] = match prog_l {
        "python" | "python2" | "python3" => &["-c"],
        "perl" => &["-e", "-E"],
        "ruby" => &["-e"],
        "node" | "nodejs" | "deno" | "bun" => &["-e", "--eval", "-p", "--print"],
        "php" => &["-r"],
        "lua" => &["-e"],
        "awk" | "gawk" | "mawk" => &["-e"],
        "osascript" => &["-e"],
        "powershell" | "pwsh" => &["-c", "-Command", "-EncodedCommand", "-e"],
        "cmd" => &["/c", "/k", "/C", "/K"],
        _ => &[],
    };
    if !code_flags.is_empty() && flag_value(cmd, code_flags).is_some() {
        out.push(Reason {
            rule: RULE_OPAQUE_INTERPRETER,
            tier: Tier::Dangerous,
            detail: format!(
                "`{prog_l}` 被要求内联执行一段代码。本引擎不解析这种语言，\
                 因此无法判断它会做什么——按最危险处理"
            ),
        });
    }
    // awk 的程序体不用 `-e` 也能给（`awk 'BEGIN{system("…")}'`），
    // 且 awk 有 system()/print > file 的执行与写入能力。
    if matches!(prog_l, "awk" | "gawk" | "mawk") && !cmd.args().is_empty() {
        out.push(Reason {
            rule: RULE_OPAQUE_INTERPRETER,
            tier: Tier::Dangerous,
            detail: "awk 程序体可经 system() 执行任意命令、经 print > 写任意文件".into(),
        });
    }
}

/// 喂给解释器的 heredoc 正文按代码判定。
///
/// `bash <<EOF … EOF` 与 `bash -c '…'` 是同一件事，只是代码从 stdin 来。
/// 不单独处理这一条，`bash <<EOF\nrm -rf /\nEOF` 就只剩「有个 heredoc」这一条
/// Write 级理由——而它真的会删掉整个根。
fn heredoc_code_rules(cmd: &SimpleCommand, prog_l: &str, out: &mut Vec<Reason>, recursion: usize) {
    let is_shell = matches!(
        prog_l,
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "ash" | "busybox"
    );
    for body in &cmd.heredoc_bodies {
        if !is_shell || recursion >= MAX_RECURSION {
            out.push(Reason {
                rule: RULE_OPAQUE_INTERPRETER,
                tier: Tier::Dangerous,
                detail: format!("`{prog_l}` 会把 heredoc 正文当代码执行，而这种语言本引擎不解析"),
            });
            continue;
        }
        match shell::parse(body) {
            Ok(inner) => {
                let v = classify_script(&inner, recursion + 1);
                out.push(Reason {
                    rule: RULE_SHELL_INLINE,
                    tier: v.tier.max(Tier::Write),
                    detail: format!("heredoc 正文经递归判定为 {}", v.tier),
                });
                out.extend(v.reasons);
            }
            Err(e) => out.push(Reason {
                rule: RULE_OPAQUE_INTERPRETER,
                tier: Tier::Dangerous,
                detail: format!("heredoc 正文无法解析（{e}）"),
            }),
        }
    }
}

/// 取某个旗标的值（`-c foo` 或 `-cfoo` 或 `--eval=foo`）。
fn flag_value<'a>(cmd: &'a SimpleCommand, flags: &[&str]) -> Option<&'a Word> {
    let args = cmd.args();
    for (i, a) in args.iter().enumerate() {
        let t = a.text.as_str();
        if flags.contains(&t) {
            return args.get(i + 1);
        }
        for f in flags {
            if let Some(rest) = t.strip_prefix(f) {
                if !rest.is_empty() && !rest.starts_with('=') {
                    return Some(a); // `-cfoo`：值粘在旗标后面
                }
                if rest.starts_with('=') {
                    return Some(a);
                }
            }
        }
    }
    None
}

/// 这个路径是不是「根」或「家目录」本身（含 glob 形态）。
///
/// `rm -rf /` 与 `rm -rf /*` 在 shell 里的效果相同，而 `/*` 归一化后不落在
/// 任何系统目录前缀之下——必须单独认。
pub fn is_rootish(p: &str) -> bool {
    let t = p.trim();
    let stripped = t
        .trim_end_matches('*')
        .trim_end_matches('.')
        .trim_end_matches('/');
    matches!(
        t,
        "/" | "/*" | "/." | "~" | "~/" | "~/*" | "$HOME" | "${HOME}" | "*"
    ) || (stripped.is_empty() && t.starts_with('/'))
        || matches!(
            crate::paths::normalize(t.trim_end_matches('*')).as_str(),
            "/" | "/~home"
        )
}

fn destructive_path_rules(cmd: &SimpleCommand, prog_l: &str, out: &mut Vec<Reason>) {
    let args = cmd.args();
    let recursive = args.iter().any(|a| is_recursive_flag(&a.text));
    let paths: Vec<&Word> = args.iter().filter(|a| !a.text.starts_with('-')).collect();

    // 块设备
    if prog_l.starts_with("mkfs")
        || matches!(
            prog_l,
            "wipefs" | "blkdiscard" | "sgdisk" | "fdisk" | "parted" | "mkswap" | "shred" | "dd"
        )
    {
        let touches_dev = args.iter().any(|a| {
            let t = a.text.strip_prefix("of=").unwrap_or(&a.text);
            crate::paths::normalize(t).starts_with("/dev/")
        });
        if touches_dev {
            out.push(Reason {
                rule: RULE_BLOCK_DEVICE,
                tier: Tier::Dangerous,
                detail: format!("`{prog_l}` 直接写块设备：整个分区/磁盘的数据会没了"),
            });
        }
    }

    // 根/家目录上的递归删除
    if matches!(prog_l, "rm" | "shred") {
        for p in &paths {
            if is_rootish(&p.text) {
                out.push(Reason {
                    rule: RULE_ROOTISH_RECURSIVE,
                    tier: Tier::Dangerous,
                    detail: format!(
                        "`{prog_l}` 的目标是 `{}`{}",
                        p.text,
                        if recursive {
                            "，且带递归旗标"
                        } else {
                            ""
                        }
                    ),
                });
            }
        }
    }

    // 目标定不了值的**不可逆**操作。
    //
    // 只对不可逆的那几个（rm/shred/mkfs/dd/wipefs）设这条，不对 cp/mv/chmod 设：
    // `rm -rf "$DIR"` 里 `$DIR` 可能是任何东西，删错了拿不回来；
    // 而 `cp "$a" "$b"` 最坏是多一个文件。对后者也设的话，凡是带变量的脚本
    // 每一行都要强确认——那会把强确认这件事本身贬值。
    if matches!(prog_l, "rm" | "shred" | "dd" | "wipefs" | "blkdiscard")
        || prog_l.starts_with("mkfs")
    {
        // 判据是 `unresolved`（值定不下来），**不是** `!is_plain()`（含展开）。
        // 用后者会把 `d=/tmp/build; rm -rf $d` 也算进来——那条我们明明还原出了
        // 完整路径，知道它删的是 /tmp/build。
        if let Some(p) = paths.iter().find(|p| p.traits.unresolved) {
            out.push(Reason {
                rule: RULE_DESTRUCTIVE_PATH,
                tier: Tier::Dangerous,
                detail: format!(
                    "`{prog_l}` 的目标 `{}` 由展开/替换决定，静态无法知道它会作用在哪里——\
                     而这个操作不可逆",
                    p.raw
                ),
            });
        }
    }

    // `mv` 会**移走**源：源本身也是被改动的一方。
    // `cp /etc/hosts ~/bak` 是备份（无害），`mv /etc/hosts ~/bak` 是把系统文件搬走（有害）。
    if prog_l == "mv" && paths.len() >= 2 {
        for src in &paths[..paths.len() - 1] {
            let class = classify_path(&src.text);
            if class != PathClass::Ordinary || is_rootish(&src.text) {
                out.push(Reason {
                    rule: RULE_DESTRUCTIVE_PATH,
                    tier: Tier::Dangerous,
                    detail: sensitive_detail(&src.text, class, "`mv` 会移走"),
                });
            }
        }
    }

    // 逐个路径参数看敏感度
    for p in &paths {
        // `dd of=x` 的路径藏在 `of=` 后面
        let raw = p.text.strip_prefix("of=").unwrap_or(&p.text);
        let class = classify_path(raw);
        if class == PathClass::Ordinary {
            continue;
        }
        // `cp`/`mv`/`ln`/`rsync`/`install` 只有**最后一个**参数是目的地；
        // 前面的是源，读源不改动它。把源也算成危险会造成大量假阳性
        // （`cp /etc/hosts ~/backup` 是备份，无害）。
        let is_copy_like = matches!(prog_l, "cp" | "mv" | "ln" | "rsync" | "install");
        if is_copy_like && !std::ptr::eq(*p, *paths.last().unwrap()) {
            continue;
        }
        // `tar`/`unzip`/`sed`/`python`/`perl` 的路径参数语义太杂，只在明确的写旗标下判
        if matches!(prog_l, "tar" | "unzip" | "python" | "perl") {
            continue;
        }
        if matches!(prog_l, "sed") && !args.iter().any(|a| a.text.starts_with("-i")) {
            continue;
        }
        let rule = if is_copy_like {
            RULE_COPY_TO_SENSITIVE
        } else {
            RULE_DESTRUCTIVE_PATH
        };
        out.push(Reason {
            rule,
            tier: Tier::Dangerous,
            detail: sensitive_detail(raw, class, &format!("`{prog_l}` 会改写")),
        });
    }

    // `chmod`/`chown` 递归改系统目录
    if matches!(prog_l, "chmod" | "chown" | "chgrp") && recursive {
        for p in &paths {
            if is_rootish(&p.text) || classify_path(&p.text) == PathClass::SystemCritical {
                out.push(Reason {
                    rule: RULE_DESTRUCTIVE_PATH,
                    tier: Tier::Dangerous,
                    detail: format!("`{prog_l}` 递归改 `{}` 的权限/归属", p.text),
                });
            }
        }
    }
}

fn is_recursive_flag(t: &str) -> bool {
    match t {
        "-r" | "-R" | "--recursive" | "-fr" | "-rf" | "-Rf" | "-fR" => true,
        // 合并的短旗标 `-rfv`
        _ => t.starts_with('-') && !t.starts_with("--") && t.contains(['r', 'R']),
    }
}

const WINDOWS_DESTRUCTIVE: &[&str] = &[
    "format", "vssadmin", "bcdedit", "cipher", "diskpart", "takeown", "icacls", "reg",
];

fn windows_rules(cmd: &SimpleCommand, prog_l: &str, out: &mut Vec<Reason>) {
    if prog_l == "del" || prog_l == "erase" || prog_l == "rd" || prog_l == "rmdir" {
        let quiet_recursive = cmd
            .args()
            .iter()
            .any(|a| matches!(a.text.to_ascii_lowercase().as_str(), "/s" | "/q"));
        let rootish = cmd.args().iter().any(|a| {
            let t = a.text.to_ascii_lowercase();
            t.ends_with(":\\") || t.ends_with(":/") || t.ends_with(":\\*") || is_rootish(&a.text)
        });
        if quiet_recursive && rootish {
            out.push(Reason {
                rule: RULE_WINDOWS_DESTRUCTIVE,
                tier: Tier::Dangerous,
                detail: format!("`{prog_l}` 静默递归删除盘根"),
            });
        }
        return;
    }
    if !WINDOWS_DESTRUCTIVE.contains(&prog_l) {
        return;
    }
    let joined = cmd
        .args()
        .iter()
        .map(|a| a.text.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let hit = match prog_l {
        // 删卷影副本 = 勒索软件的标准前置动作
        "vssadmin" | "diskpart" => joined.contains("delete"),
        "reg" => joined.contains("delete"),
        "format" | "cipher" | "bcdedit" => true,
        "takeown" | "icacls" => joined.contains(":\\") || joined.contains("/t"),
        _ => false,
    };
    if hit {
        out.push(Reason {
            rule: RULE_WINDOWS_DESTRUCTIVE,
            tier: Tier::Dangerous,
            detail: format!("`{prog_l}` 是系统级破坏/持久化操作"),
        });
    }
}

/// fork 炸弹：函数体里递归调用自己，且以后台方式派生。
fn fork_bomb_rules(cmd: &SimpleCommand, script: &Script, out: &mut Vec<Reason>) {
    if script.functions.is_empty() || !script.traits.background {
        return;
    }
    let Some(prog) = cmd.program() else { return };
    if cmd.depth == 0 || !script.functions.iter().any(|f| f == prog) {
        return;
    }
    // 同一条管道里同一个函数出现 ≥2 次 = 自我复制
    let self_pipe = script.pipelines.iter().any(|p| {
        p.stages
            .iter()
            .filter(|s| s.program() == Some(prog))
            .count()
            >= 2
    });
    if self_pipe {
        out.push(Reason {
            rule: RULE_FORK_BOMB,
            tier: Tier::Dangerous,
            detail: format!("函数 `{prog}` 在自己体内以后台方式反复复制自己：fork 炸弹"),
        });
    }
}

// ══════════════════════════════════════════════════════════════════════
// 只读白名单
// ══════════════════════════════════════════════════════════════════════

/// 无条件只读的程序。
///
/// 入选标准是**没有执行与写入能力**。刻意排除的：
/// - `sed`/`awk`：`sed -i` 就地改文件，awk 能 `system()`；
/// - `find`：`-exec`/`-delete` 能删能跑（下面单独按旗标放行）；
/// - `less`/`more`/`vi`/`vim`/`nano`/`man`：交互式，`!cmd` 能开 shell；
/// - `env`/`xargs`/`sudo`：包装器，跑的是别人（已在 peel 处理）；
/// - `tar`/`unzip`：会往磁盘写文件；
/// - `mount`/`umount`：改系统状态。
const READ_ONLY: &[&str] = &[
    "ls",
    "ll",
    "dir",
    "cat",
    "tac",
    "head",
    "tail",
    "wc",
    "nl",
    "od",
    "strings",
    "file",
    "stat",
    "du",
    "df",
    "pwd",
    "whoami",
    "id",
    "groups",
    "date",
    "uptime",
    "hostname",
    "hostnamectl",
    "uname",
    "arch",
    "nproc",
    "lscpu",
    "lsmem",
    "lsblk",
    "lsusb",
    "lspci",
    "free",
    "vmstat",
    "iostat",
    "mpstat",
    "pidstat",
    "sar",
    "ps",
    "pgrep",
    "top",
    "htop",
    "btop",
    "atop",
    "iotop",
    "lsof",
    "dmesg",
    "echo",
    "printf",
    "true",
    "false",
    "yes",
    "seq",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "which",
    "type",
    "whereis",
    "command",
    "printenv",
    "locale",
    "tty",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ack",
    "sort",
    "uniq",
    "cut",
    "paste",
    "join",
    "comm",
    "diff",
    "cmp",
    "colordiff",
    "column",
    "tr",
    "expand",
    "unexpand",
    "fold",
    "rev",
    "cksum",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "sha512sum",
    "b2sum",
    "tree",
    "jq",
    "yq",
    "xxd",
    "hexdump",
    "ip",
    "ifconfig",
    "netstat",
    "ss",
    "arp",
    "route",
    "ping",
    "ping6",
    "traceroute",
    "tracepath",
    "dig",
    "nslookup",
    "host",
    "getent",
    "curl",
    "wget",
    "w",
    "who",
    "last",
    "lastlog",
    "finger",
    "sleep",
    "time",
    "cal",
    "bc",
    "expr",
    "test",
    "wait",
    "jobs",
    "history",
    "alias",
    "clear",
];

/// 按子命令放行的程序：只有**明确只读**的子命令才算。
fn subcommand_read_only(prog: &str, args: &[Word]) -> Option<bool> {
    // 子命令通常是第一个非旗标参数（`git --no-pager log` → `log`），
    // 但有些工具的子命令**本身就是旗标**（`crontab -l`）。取不到非旗标参数时退回首参，
    // 否则 `crontab -l` 会在这里返回 None、落到程序名白名单（crontab 不在里面）→ write。
    let first = args
        .iter()
        .find(|a| !a.text.starts_with('-'))
        .or_else(|| args.first())?
        .text
        .as_str();
    let ok: &[&str] = match prog {
        "git" => &[
            "status",
            "log",
            "diff",
            "show",
            "blame",
            "describe",
            "rev-parse",
            "ls-files",
            "ls-tree",
            "shortlog",
            "reflog",
            "cat-file",
            "grep",
            "whatchanged",
            "annotate",
            "count-objects",
            "verify-commit",
        ],
        "docker" => &[
            "ps", "images", "logs", "inspect", "version", "info", "top", "port", "stats", "history",
        ],
        "podman" => &[
            "ps", "images", "logs", "inspect", "version", "info", "top", "port", "stats",
        ],
        "kubectl" => &[
            "get",
            "describe",
            "logs",
            "top",
            "version",
            "explain",
            "api-resources",
            "cluster-info",
        ],
        "systemctl" => &[
            "status",
            "list-units",
            "list-unit-files",
            "is-active",
            "is-enabled",
            "show",
            "cat",
            "get-default",
        ],
        "journalctl" => return Some(true),
        "apt" | "apt-get" | "dnf" | "yum" | "pacman" | "brew" | "zypper" => {
            &["list", "search", "show", "info", "policy"]
        }
        "pip" | "pip3" => &["list", "show", "freeze", "search"],
        "npm" => &["ls", "list", "view", "info", "outdated", "config"],
        "cargo" => &["tree", "metadata", "search", "--version"],
        "crontab" => &["-l"],
        _ => return None,
    };
    Some(ok.contains(&first))
}

/// `find` 的只读性取决于它有没有带执行/删除动作。
fn find_is_read_only(args: &[Word]) -> bool {
    const ACTIONS: &[&str] = &[
        "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fls", "-fprint", "-fprint0", "-fprintf",
        "-D",
    ];
    !args.iter().any(|a| ACTIONS.contains(&a.text.as_str()))
}

/// 这条简单命令本身是否只读。
///
/// 只回答「这一条」；整体是否 read_only 由 [`classify_script`] 按「所有组分都命中」决定。
pub fn is_read_only(cmd: &SimpleCommand) -> bool {
    // 纯赋值：不跑任何程序，视为只读（真正的风险在用到它的地方，那里会另行判定）
    if cmd.argv.is_empty() {
        return true;
    }
    // 有写重定向就不是只读，无论程序是什么
    // 有写重定向就不是只读——但丢弃式伪设备除外（见 is_discard_sink）
    if cmd
        .redirects
        .iter()
        .any(|r| r.kind.writes() && !crate::paths::is_discard_sink(&r.target.text))
    {
        return false;
    }
    // 词不是普通字面量 → 永不 read_only（§4.3 硬约束 2）
    if !cmd.all_words_plain() {
        return false;
    }
    let Some(prog) = cmd.program_basename() else {
        return false;
    };
    let prog = prog.to_ascii_lowercase();
    let prog = prog.trim_end_matches(".exe");
    if prog == "find" || prog == "fd" {
        return find_is_read_only(cmd.args());
    }
    // `curl`/`wget` 取内容打到 stdout 是只读的，但带上落地旗标就是往磁盘写文件。
    // 不区分这一点，`curl http://x -o /tmp/payload.sh` 会因为「curl 在白名单里」
    // 而被**自动放行**——那是这张白名单最危险的一个洞。
    if matches!(prog, "curl" | "wget") {
        const WRITES: &[&str] = &[
            "-o",
            "-O",
            "--output",
            "--output-document",
            "--remote-name",
            "--create-dirs",
            "-P",
            "--directory-prefix",
        ];
        return !cmd.args().iter().any(|a| {
            let t = a.text.as_str();
            WRITES.contains(&t) || t.starts_with("--output=") || t.starts_with("-o")
        });
    }
    if let Some(ok) = subcommand_read_only(prog, cmd.args()) {
        return ok;
    }
    READ_ONLY.contains(&prog)
}

/// 传输路径分级（§4.3「文件传输同等受分级」）。
///
/// 单独一个入口是因为调用方拿到的不是命令字符串而是路径——
/// `sftp_get`/`sftp_put` 与 MCP 的文件工具都走这里。
pub fn classify_transfer(local_path: &str) -> Verdict {
    let class = classify_path(local_path);
    match class {
        PathClass::Ordinary => Verdict::one(
            RULE_WRITE_REDIRECT,
            Tier::Write,
            format!("文件将写入 `{local_path}`"),
        ),
        _ => Verdict::one(
            RULE_WRITE_REDIRECT_SENSITIVE,
            Tier::Dangerous,
            sensitive_detail(local_path, class, "文件将写入"),
        ),
    }
}

/// 解析错误也要能被上层区分（供 UI 说「看不懂」而不是「危险」）。
pub fn parse_error_of(input: &str) -> Option<ParseError> {
    shell::parse(input).err()
}
