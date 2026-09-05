//! 补丁盘点（M7.1）：列出各发行版**可升级**的包。**只读**——不在程序里执行升级。
//!
//! # 为什么只读
//!
//! 升级要 root，而 root 要 `sudo`，`sudo` 要 TTY——命令通道 (`app/src/agent/exec_port.rs`)
//! 走 `channel.exec()`，全程没有 `request_pty`，`sudo` 在那条路上必然快速失败。
//! 三条出路（NOPASSWD / 弹框收口令注入 PTY / 把命令送进用户自己的终端）安全含义完全不同，
//! 用户 2026-08-28 裁定先只做只读盘点，升级动作把命令送进终端由他按回车。
//!
//! # 「0 个可更新」与「没有这个包管理器」是两种结果
//!
//! 出口原文点名了这一条。压成同一个空列表的话，一台 Alpine 机器会显示「0 个可更新」——
//! 用户据此以为系统是最新的，而真相是我们根本没查。故命令首行输出探测标记，
//! 解析成 [`PackageScan`] 的两个变体。（同一条纪律见 [`crate::services`] 的 systemd 探测。）
//!
//! # 为什么解析在 Rust 而不是在 shell 里归一
//!
//! 让远端 shell 把五种输出格式统一成一种确实更省事，但那段 awk/sed 没有任何测试能覆盖，
//! 且它跑在**目标机**上——版本差异、locale、busybox 与 GNU 的差别全都落在一段不可测的脚本里。
//! 现在 shell 只负责「探测 + 原样吐出」，五个解析器各自在这里带单测。

use serde::{Deserialize, Serialize};

/// 已知的包管理器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageManager {
    Apt,
    Dnf,
    Zypper,
    Apk,
    Pacman,
}

impl PackageManager {
    /// 稳定串（探测标记、审计、前端显示共用）。
    pub fn as_str(self) -> &'static str {
        match self {
            PackageManager::Apt => "apt",
            PackageManager::Dnf => "dnf",
            PackageManager::Zypper => "zypper",
            PackageManager::Apk => "apk",
            PackageManager::Pacman => "pacman",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "apt" => PackageManager::Apt,
            "dnf" => PackageManager::Dnf,
            "zypper" => PackageManager::Zypper,
            "apk" => PackageManager::Apk,
            "pacman" => PackageManager::Pacman,
            _ => return None,
        })
    }

    /// 用户要自己执行的升级命令（**本程序不跑它**，只显示出来让用户送进终端）。
    pub fn upgrade_hint(self) -> &'static str {
        match self {
            PackageManager::Apt => "sudo apt-get update && sudo apt-get upgrade",
            PackageManager::Dnf => "sudo dnf upgrade",
            PackageManager::Zypper => "sudo zypper update",
            PackageManager::Apk => "sudo apk upgrade",
            PackageManager::Pacman => "sudo pacman -Syu",
        }
    }
}

/// 一个可升级的包。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageUpdate {
    pub name: String,
    /// 当前已装版本。部分管理器的列表命令不给它（如 dnf 的 check-update）。
    pub current: Option<String>,
    /// 可升级到的版本。
    pub candidate: String,
}

/// 盘点结果。两个变体的区分见模块头注。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageScan {
    /// 五种都没找到——本功能在这台机器上不适用，**不是**「已是最新」。
    NoManager,
    /// 找到了；`updates` 为空才是真的「已是最新」。
    Updates {
        manager: PackageManager,
        updates: Vec<PackageUpdate>,
    },
}

const MGR_MARK: &str = "MGR=";
const RAW_MARK: &str = "RAW=";

/// 盘点命令。**只读**：五条子命令都只查询、不改任何状态、不联网装包。
///
/// · apt：`apt list --upgradable` 读的是本地已同步的索引（不跑 `apt update`，那会写 /var/lib）；
/// · dnf：`check-update` 有更新时退出码是 100，故末尾 `|| true`，否则整条命令被判失败；
/// · apk：`version -l '<'` 只比较版本；
/// · pacman：`-Qu` 查本地库，不同步。
///
/// 每行原样加 `RAW=` 前缀吐出（理由见模块头注：归一放在 Rust 里，可测）。
pub fn scan_command() -> &'static str {
    concat!(
        "if command -v apt >/dev/null 2>&1; then echo MGR=apt; ",
        "apt list --upgradable 2>/dev/null | sed 's/^/RAW=/'; ",
        "elif command -v dnf >/dev/null 2>&1; then echo MGR=dnf; ",
        "{ dnf -q check-update 2>/dev/null || true; } | sed 's/^/RAW=/'; ",
        "elif command -v zypper >/dev/null 2>&1; then echo MGR=zypper; ",
        "zypper -q list-updates 2>/dev/null | sed 's/^/RAW=/'; ",
        "elif command -v apk >/dev/null 2>&1; then echo MGR=apk; ",
        "apk version -l '<' 2>/dev/null | sed 's/^/RAW=/'; ",
        "elif command -v pacman >/dev/null 2>&1; then echo MGR=pacman; ",
        "{ pacman -Qu 2>/dev/null || true; } | sed 's/^/RAW=/'; ",
        "else echo MGR=none; fi"
    )
}

/// 解析 [`scan_command`] 的 stdout。绝不 panic。
pub fn parse_scan(stdout: &str) -> PackageScan {
    let mut manager = None;
    let mut raw: Vec<&str> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(v) = line.trim().strip_prefix(MGR_MARK) {
            manager = PackageManager::from_str(v.trim());
            continue;
        }
        if let Some(v) = line.strip_prefix(RAW_MARK) {
            raw.push(v);
        }
    }
    let Some(manager) = manager else {
        return PackageScan::NoManager;
    };
    let updates = match manager {
        PackageManager::Apt => parse_apt(&raw),
        PackageManager::Dnf => parse_dnf(&raw),
        PackageManager::Zypper => parse_zypper(&raw),
        PackageManager::Apk => parse_apk(&raw),
        PackageManager::Pacman => parse_pacman(&raw),
    };
    PackageScan::Updates { manager, updates }
}

/// `apt list --upgradable`：
/// `nginx/jammy-updates 1.18.0-6ubuntu14.4 amd64 [upgradable from: 1.18.0-6ubuntu14.3]`
pub fn parse_apt(lines: &[&str]) -> Vec<PackageUpdate> {
    let mut out = Vec::new();
    for l in lines {
        let l = l.trim();
        // 首行是 `Listing...`；本地化环境下是别的词——判据取「有没有 /」而不是匹配那个词。
        let Some((name, rest)) = l.split_once('/') else {
            continue;
        };
        let mut it = rest.split_whitespace();
        let _suite = it.next();
        let Some(candidate) = it.next() else { continue };
        // `[upgradable from: X]`；拿不到就 None，不编。
        let current = l
            .split_once("upgradable from:")
            .map(|(_, v)| v.trim().trim_end_matches(']').trim().to_string())
            .filter(|s| !s.is_empty());
        out.push(PackageUpdate {
            name: name.trim().to_string(),
            current,
            candidate: candidate.to_string(),
        });
    }
    out
}

/// `dnf -q check-update`：`bash.x86_64    5.1.8-6.el9_1   baseos`
///
/// **必须在 `Obsoleting Packages` 之后停下**：那一段列的是「被淘汰的包」，格式相同但语义
/// 完全不同（把它算成可升级会凭空多出一堆条目）。
pub fn parse_dnf(lines: &[&str]) -> Vec<PackageUpdate> {
    let mut out = Vec::new();
    for l in lines {
        let t = l.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with("Obsoleting") {
            break;
        }
        // 三列：NAME.ARCH  VERSION  REPO。少于三列的是提示行。
        let cols: Vec<&str> = t.split_whitespace().collect();
        if cols.len() < 3 {
            continue;
        }
        // 去掉 .arch 后缀（`bash.x86_64` → `bash`）；没有点就原样。
        let name = cols[0].rsplit_once('.').map_or(cols[0], |(n, _)| n);
        out.push(PackageUpdate {
            name: name.to_string(),
            current: None, // check-update 不给已装版本，不猜
            candidate: cols[1].to_string(),
        });
    }
    out
}

/// `zypper -q list-updates`：表格形式
/// `v | repo-oss | bash | 5.1.8-1.1 | 5.1.8-2.1 | x86_64`
pub fn parse_zypper(lines: &[&str]) -> Vec<PackageUpdate> {
    let mut out = Vec::new();
    for l in lines {
        let t = l.trim();
        // 表头分隔行（`--+---+---`）与表头本身跳过
        if t.is_empty() || t.starts_with("--") || t.starts_with("S |") || t.starts_with("S  |") {
            continue;
        }
        let cols: Vec<&str> = t.split('|').map(str::trim).collect();
        if cols.len() < 5 {
            continue;
        }
        if cols[2].eq_ignore_ascii_case("Name") {
            continue; // 本地化表头兜底
        }
        out.push(PackageUpdate {
            name: cols[2].to_string(),
            current: Some(cols[3].to_string()).filter(|s| !s.is_empty()),
            candidate: cols[4].to_string(),
        });
    }
    out
}

/// `apk version -l '<'`：`busybox-1.36.1-r5 < 1.36.1-r7`
///
/// 名字与版本之间没有分隔符——apk 的包名可以含 `-`，所以**从右往左**剥两段
/// （`-r5` 与 `1.36.1`）才是版本，剩下的是名字。
pub fn parse_apk(lines: &[&str]) -> Vec<PackageUpdate> {
    let mut out = Vec::new();
    for l in lines {
        let t = l.trim();
        // 首行是 `Installed:   Available:` 之类的表头
        if t.is_empty() || t.starts_with("Installed:") {
            continue;
        }
        let Some((left, right)) = t.split_once('<') else {
            continue;
        };
        let installed = left.trim();
        let candidate = right.trim();
        if installed.is_empty() || candidate.is_empty() {
            continue;
        }
        // `name-1.36.1-r5` → 名字是去掉最后两段（版本 + revision）
        let (name, current) = split_apk_name(installed);
        out.push(PackageUpdate {
            name,
            current,
            candidate: candidate.to_string(),
        });
    }
    out
}

/// `name-1.2.3-r4` → (`name`, `Some("1.2.3-r4")`)。剥不出来就整串当名字、版本 None。
fn split_apk_name(s: &str) -> (String, Option<String>) {
    // 从右数第二个 `-` 是版本起点（最后一段是 `rN` revision）
    let Some(rev_at) = s.rfind('-') else {
        return (s.to_string(), None);
    };
    let Some(ver_at) = s[..rev_at].rfind('-') else {
        return (s.to_string(), None);
    };
    (s[..ver_at].to_string(), Some(s[ver_at + 1..].to_string()))
}

/// `pacman -Qu`：`bash 5.1.016-1 -> 5.2.015-1`
pub fn parse_pacman(lines: &[&str]) -> Vec<PackageUpdate> {
    let mut out = Vec::new();
    for l in lines {
        let t = l.trim();
        if t.is_empty() {
            continue;
        }
        let Some((left, right)) = t.split_once("->") else {
            continue;
        };
        let mut it = left.split_whitespace();
        let (Some(name), current) = (it.next(), it.next()) else {
            continue;
        };
        let candidate = right.trim();
        if candidate.is_empty() {
            continue;
        }
        out.push(PackageUpdate {
            name: name.to_string(),
            current: current.map(str::to_string),
            candidate: candidate.to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 出口原文点名的那一条：两种结果不得混成一个空列表。
    #[test]
    fn no_manager_is_not_zero_updates() {
        assert_eq!(parse_scan("MGR=none\n"), PackageScan::NoManager);
        assert_eq!(
            parse_scan("MGR=apt\nRAW=Listing...\n"),
            PackageScan::Updates {
                manager: PackageManager::Apt,
                updates: vec![],
            }
        );
        // 标记整个缺失（命令没跑起来）也按「不适用」处理，不冒充成「已是最新」
        assert_eq!(parse_scan(""), PackageScan::NoManager);
    }

    #[test]
    fn apt_real_output() {
        let out = "MGR=apt\n\
            RAW=Listing...\n\
            RAW=nginx/jammy-updates 1.18.0-6ubuntu14.4 amd64 [upgradable from: 1.18.0-6ubuntu14.3]\n\
            RAW=openssl/jammy-security 3.0.2-0ubuntu1.15 amd64 [upgradable from: 3.0.2-0ubuntu1.12]\n";
        let PackageScan::Updates { manager, updates } = parse_scan(out) else {
            panic!()
        };
        assert_eq!(manager, PackageManager::Apt);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].name, "nginx");
        assert_eq!(updates[0].candidate, "1.18.0-6ubuntu14.4");
        assert_eq!(updates[0].current.as_deref(), Some("1.18.0-6ubuntu14.3"));
    }

    #[test]
    fn dnf_real_output_stops_at_obsoleting() {
        let lines = [
            "",
            "bash.x86_64            5.1.8-6.el9_1        baseos",
            "curl.x86_64            7.76.1-23.el9        baseos",
            "",
            "Obsoleting Packages",
            "kernel.x86_64          5.14.0-362.el9       baseos",
        ];
        let u = parse_dnf(&lines);
        assert_eq!(u.len(), 2, "Obsoleting 段被算进来了：{u:?}");
        assert_eq!(u[0].name, "bash"); // .arch 后缀剥掉
        assert_eq!(u[0].candidate, "5.1.8-6.el9_1");
        assert_eq!(u[0].current, None, "check-update 不给已装版本，不该编一个");
    }

    #[test]
    fn zypper_real_output() {
        let lines = [
            "S | Repository | Name | Current Version | Available Version | Arch",
            "--+------------+------+-----------------+-------------------+-------",
            "v | repo-oss   | bash | 5.1.8-1.1       | 5.1.8-2.1         | x86_64",
        ];
        let u = parse_zypper(&lines);
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].name, "bash");
        assert_eq!(u[0].current.as_deref(), Some("5.1.8-1.1"));
        assert_eq!(u[0].candidate, "5.1.8-2.1");
    }

    #[test]
    fn apk_real_output() {
        let lines = [
            "Installed:                Available:",
            "busybox-1.36.1-r5       < 1.36.1-r7",
            "ca-certificates-bundle-20240226-r0 < 20241010-r0",
        ];
        let u = parse_apk(&lines);
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].name, "busybox");
        assert_eq!(u[0].current.as_deref(), Some("1.36.1-r5"));
        assert_eq!(u[0].candidate, "1.36.1-r7");
        // 名字里带 `-` 的包：从右剥两段才对，否则名字会被切碎
        assert_eq!(u[1].name, "ca-certificates-bundle");
        assert_eq!(u[1].current.as_deref(), Some("20240226-r0"));
    }

    #[test]
    fn pacman_real_output() {
        let lines = ["bash 5.1.016-1 -> 5.2.015-1", "curl 8.4.0-1 -> 8.5.0-1"];
        let u = parse_pacman(&lines);
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].name, "bash");
        assert_eq!(u[0].current.as_deref(), Some("5.1.016-1"));
        assert_eq!(u[0].candidate, "5.2.015-1");
    }

    /// 五个解析器对垃圾输入一律跳过，绝不 panic（输出来自远端，形状不可尽信）。
    #[test]
    fn every_parser_survives_garbage() {
        let junk = [
            "",
            "   ",
            "|||",
            "-> ",
            "<",
            "a",
            "\u{4e2d}\u{6587}",
            "x y",
            "name/",
        ];
        assert!(parse_apt(&junk).iter().all(|u| !u.name.is_empty()));
        assert!(parse_dnf(&junk).iter().all(|u| !u.name.is_empty()));
        assert!(parse_zypper(&junk).iter().all(|u| !u.name.is_empty()));
        assert!(parse_apk(&junk).iter().all(|u| !u.name.is_empty()));
        assert!(parse_pacman(&junk).iter().all(|u| !u.name.is_empty()));
    }

    /// 盘点命令必须是只读的：一条会改远端状态的子命令都不许有。
    #[test]
    fn scan_command_is_read_only() {
        let c = scan_command();
        for forbidden in [
            "apt-get upgrade",
            "apt upgrade",
            "dnf upgrade",
            "dnf update",
            "zypper update",
            "apk upgrade",
            "pacman -Syu",
            "apt update",
            "apt-get update",
            "zypper refresh",
            "pacman -Sy",
        ] {
            assert!(
                !c.contains(forbidden),
                "盘点命令里出现了会改远端状态的 {forbidden:?}"
            );
        }
        // 五种都要探测到，否则某个发行版会静默落进「不适用」
        for m in ["apt", "dnf", "zypper", "apk", "pacman"] {
            assert!(c.contains(&format!("command -v {m}")), "漏了 {m} 的探测");
        }
    }

    /// dnf 的 check-update 在「有更新」时退出码是 100——不兜住的话整条命令被判失败，
    /// 用户在有更新的机器上看到的是一条报错，没更新时反而正常。
    #[test]
    fn dnf_branch_tolerates_exit_100() {
        let c = scan_command();
        let dnf_at = c.find("dnf -q check-update").expect("dnf 分支不见了");
        assert!(
            c[dnf_at..].starts_with("dnf -q check-update 2>/dev/null || true"),
            "dnf 分支没兜住退出码 100"
        );
    }

    #[test]
    fn upgrade_hints_are_shown_not_run() {
        // 这些串只进 UI 让用户自己送进终端；本模块任何命令构造器都不得包含它们
        for m in [
            PackageManager::Apt,
            PackageManager::Dnf,
            PackageManager::Zypper,
            PackageManager::Apk,
            PackageManager::Pacman,
        ] {
            let hint = m.upgrade_hint();
            assert!(hint.starts_with("sudo "), "{hint}");
            assert!(!scan_command().contains(hint), "升级命令混进了盘点命令");
        }
    }
}
