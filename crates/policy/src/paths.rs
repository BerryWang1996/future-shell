//! 敏感路径判定（总设计 §4.3 永久黑名单的路径半边 + §4.3「文件传输同等受分级」）。
//!
//! # 两个调用方，同一套判据
//!
//! 1. **命令里的写目标**：重定向 `> path`、`tee path`、`cp/mv/install ... path`、`sed -i path`。
//! 2. **文件传输的本地目的路径**：`sftp_get` 落到本地磁盘的那个路径，以及任何触及本地磁盘的
//!    MCP 工具。§4.3 说得很直白——remote 内容 → 本地磁盘是**信任边界跨越**，
//!    策略引擎看到的是路径而非「文件传输」，所以必须与命令字符串同等受管。
//!
//! 把两者收在同一个模块里，是为了让「哪些路径算危险」只有一处定义。分两处写，
//! 迟早会出现「命令路径挡住了、传输路径没挡」这种半边防护。
//!
//! # 归一化是绕过面，不是清理工作
//!
//! 下面每一条都指向同一个文件，只认字面前缀的判据会漏掉除第一条以外的全部：
//!
//! ```text
//! /etc/passwd
//! /etc//passwd
//! /etc/./passwd
//! /etc/../etc/passwd
//! //etc/passwd
//! ~/../.ssh/authorized_keys      （= $HOME 的兄弟目录下的 .ssh，通常不是家目录，但可能是）
//! ~/.ssh/../.ssh/authorized_keys
//! ```
//!
//! 所以判定前必须做**词法**归一化（不碰文件系统——策略引擎跑在本地，
//! 而路径可能指远端，`fs::canonicalize` 既不可用也不该用：它会跟随软链，
//! 那意味着判定结果依赖于当前磁盘状态，同一条命令在两台机器上分级不同）。

use crate::Tier;

/// 路径的敏感类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathClass {
    /// 普通路径：没有已知的特殊含义。
    Ordinary,
    /// 系统关键位置：`/`、`/etc`、`/boot`、`/dev/*`、`/sys`、`/proc`、系统可执行目录。
    SystemCritical,
    /// 写进去就等于**获得代码执行**：shell rc、`~/.ssh/**`、自启动位置、cron、服务单元。
    ///
    /// 与 `SystemCritical` 分开是因为它们的危险来源不同：前者是「弄坏系统」，
    /// 后者是「下次登录/开机时替你跑代码」。后者往往不需要 root，也更隐蔽——
    /// 一次 `sftp_get` 到 `~/.bashrc` 不会有任何报错，用户下一次开终端才中招。
    CodeExecution,
}

impl PathClass {
    /// 写入这类路径对应的风险级别。
    ///
    /// 只有 `Ordinary` 是 `Write`；另两类一律 `Dangerous`（§4.3「强确认」）。
    pub fn write_tier(self) -> Tier {
        match self {
            Self::Ordinary => Tier::Write,
            Self::SystemCritical | Self::CodeExecution => Tier::Dangerous,
        }
    }
}

/// 词法归一化：统一分隔符、折叠重复分隔符、消掉 `.` 与 `..`、展开 `~` 与 `$HOME`。
///
/// 刻意**不**访问文件系统（不 canonicalize、不 stat）：
/// - 路径可能指的是**远端**主机，本地根本没有这个文件；
/// - canonicalize 会跟随软链，于是同一条命令在两台机器上分级不同——
///   策略判定必须是输入的纯函数，否则无法测试、也无法向用户解释。
///
/// 返回的路径用 `/` 作分隔符、不带尾斜杠（根目录除外）。
pub fn normalize(path: &str) -> String {
    // Windows 盘符前缀单独留着：`C:/Windows` 的 `C:` 不是路径段
    let (prefix, rest) = split_windows_prefix(path);
    let unified = rest.replace('\\', "/");
    let expanded = expand_home(&unified);

    let absolute = expanded.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in expanded.split('/') {
        match seg {
            // 空段来自 `//` 与首尾斜杠
            "" | "." => {}
            ".." => {
                // 绝对路径下 `/..` 就是 `/`；相对路径下要保留无法消掉的 `..`
                match out.last() {
                    Some(&last) if last != ".." => {
                        out.pop();
                    }
                    _ if absolute => {}
                    _ => out.push(".."),
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if !prefix.is_empty() {
        format!("{}/{}", prefix.trim_end_matches('/'), joined)
    } else if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// 切出 Windows 盘符或 UNC 前缀。
fn split_windows_prefix(p: &str) -> (&str, &str) {
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && (b[0] as char).is_ascii_alphabetic() {
        return (&p[..2], &p[2..]);
    }
    if p.starts_with("\\\\") || p.starts_with("//") {
        // UNC `\\server\share\...`：前两段是主机与共享名，不参与 `..` 消解
        return ("", p);
    }
    ("", p)
}

/// 把 `~`、`$HOME`、`%USERPROFILE%` 换成一个**统一的占位家目录**。
///
/// 用占位符而不是读环境变量，理由同上：判定必须是输入的纯函数。
/// 占位选 `/~home`——它不可能与真实路径撞名（真实路径里不会有 `/~home` 这一段），
/// 因此下面的家目录规则可以安全地按前缀匹配。
const HOME: &str = "/~home";

fn expand_home(p: &str) -> String {
    // `~` 只在路径开头才是家目录（`a~b` 里的 `~` 是普通字符）
    let p = if p == "~" {
        HOME.to_string()
    } else if let Some(rest) = p.strip_prefix("~/") {
        format!("{HOME}/{rest}")
    } else {
        p.to_string()
    };
    // `$HOME` / `${HOME}` / `%USERPROFILE%` 同义
    let p = p.replace("${HOME}", HOME).replace("$HOME", HOME);
    let lower = p.to_ascii_lowercase();
    if let Some(at) = lower.find("%userprofile%") {
        return format!("{}{}{}", &p[..at], HOME, &p[at + "%userprofile%".len()..]);
    }
    p
}

/// 系统关键目录前缀（归一化后、以 `/` 开头）。
const SYSTEM_CRITICAL_PREFIXES: &[&str] = &[
    "/boot",
    "/dev",
    "/etc",
    "/proc",
    "/sys",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/usr/bin",
    "/usr/sbin",
    "/usr/lib",
    "/System",
    "/Library/LaunchDaemons",
    "/Windows",
    "/windows",
];

/// 写进去即获得代码执行的**具体文件名**（在家目录下匹配）。
const HOME_CODE_EXEC_FILES: &[&str] = &[
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".kshrc",
    ".cshrc",
    ".tcshrc",
    ".login",
    ".inputrc",
    ".xinitrc",
    ".xprofile",
    ".vimrc",
    ".gitconfig",
];

/// 写进去即获得代码执行的**目录**（归一化后前缀匹配；`{HOME}` 会被替换成占位家目录）。
const CODE_EXEC_DIR_PREFIXES: &[&str] = &[
    "{HOME}/.ssh",
    "{HOME}/.config/autostart",
    "{HOME}/.config/systemd/user",
    "{HOME}/.local/share/systemd/user",
    "{HOME}/Library/LaunchAgents",
    "{HOME}/bin",
    "{HOME}/.local/bin",
    "{HOME}/.oh-my-zsh",
    "{HOME}/.bashrc.d",
    "{HOME}/.zshrc.d",
    "{HOME}/AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup",
    "{HOME}/Documents/WindowsPowerShell",
    "{HOME}/Documents/PowerShell",
    "/etc/systemd/system",
    "/etc/cron.d",
    "/etc/cron.daily",
    "/etc/cron.hourly",
    "/etc/cron.weekly",
    "/etc/cron.monthly",
    "/etc/profile.d",
    "/etc/init.d",
    "/var/spool/cron",
    "/Library/LaunchAgents",
    "/Library/LaunchDaemons",
    "/Library/StartupItems",
    "/ProgramData/Microsoft/Windows/Start Menu/Programs/StartUp",
];

/// `/dev` 下的**伪设备**：写进去什么也不会坏。
///
/// 必须豁免，否则 `cmd > /dev/null 2>&1` 这种到处都有的写法会被判成
/// 「写入系统关键位置」——那是纯假阳性，而假阳性会把用户训练成闭眼点确认。
/// 真正危险的是块设备（`/dev/sda`），它们不在这张表里。
const DEV_PSEUDO: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/stdin",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/tty",
    "/dev/console",
    "/dev/ptmx",
];

/// 这个路径是不是「写进去等于丢掉」的伪设备。
///
/// 与 [`classify_path`] 返回 `Ordinary` 不是一回事：`Ordinary` 的意思是「普通文件，
/// 写它算一次写操作」，而这些路径**根本没有写操作发生**——
/// `ls > /dev/null` 与 `ls` 对系统的影响完全相同。
/// 不区分这一点，`cmd > /dev/null 2>&1` 这种随处可见的写法就永远拿不到 read_only。
pub fn is_discard_sink(path: &str) -> bool {
    let n = normalize(path).to_ascii_lowercase();
    DEV_PSEUDO.contains(&n.as_str()) || n.starts_with("/dev/fd/")
}

/// 判定一个路径的敏感类别。
pub fn classify_path(path: &str) -> PathClass {
    let n = normalize(path);
    // 根目录本身
    if n == "/" {
        return PathClass::SystemCritical;
    }
    // 伪设备与 `/dev/fd/N`：写它们不构成破坏
    let low = n.to_ascii_lowercase();
    if DEV_PSEUDO.contains(&low.as_str()) || low.starts_with("/dev/fd/") {
        return PathClass::Ordinary;
    }

    // 代码执行类先判：`/etc/cron.d` 既在 `/etc` 之下也是代码执行位置，
    // 两者都是 Dangerous，但归类要指向更准确的那个原因（审计与 UI 要说清为什么）。
    for dir in CODE_EXEC_DIR_PREFIXES {
        let dir = dir.replace("{HOME}", HOME);
        if under(&n, &dir) {
            return PathClass::CodeExecution;
        }
    }
    // 家目录下的 rc 文件
    if let Some(rel) = n.strip_prefix(HOME).and_then(|r| r.strip_prefix('/')) {
        // 只认**直接**位于家目录下的 rc 文件（`~/.bashrc`），
        // 不认 `~/proj/.bashrc`——那是项目里的一份样例文件，写它不会获得执行
        if !rel.contains('/') && HOME_CODE_EXEC_FILES.contains(&rel) {
            return PathClass::CodeExecution;
        }
    }
    // PowerShell $PROFILE 的常见落点（大小写不敏感匹配文件名）
    if n.to_ascii_lowercase()
        .ends_with("/microsoft.powershell_profile.ps1")
    {
        return PathClass::CodeExecution;
    }

    for pre in SYSTEM_CRITICAL_PREFIXES {
        if under(&n, pre) {
            return PathClass::SystemCritical;
        }
    }
    PathClass::Ordinary
}

/// `path` 是否就是 `dir` 或位于 `dir` 之下。
///
/// 三个细节各自对应一类绕过或假阳性：
///
/// 1. **按段比较，不用裸前缀**：`/etc` 的裸前缀会把 `/etcetera` 也算进去（假阳性），
///    而假阳性会让用户对无害操作反复点确认。
/// 2. **大小写不敏感**：三个目标平台里 Windows 与 macOS 默认都不敏感。
/// 3. **盘符要能被跳过**：规则表里写的是 `/Windows`，而真实输入是 `C:\Windows\...`
///    ——归一化后成 `C:/Windows/...`，不脱掉 `C:` 就永远匹配不上。
///    第一版漏了这一条，`C:\Windows\System32` 被判成普通路径。
fn under(path: &str, dir: &str) -> bool {
    let d = dir.to_ascii_lowercase();
    let p = path.to_ascii_lowercase();
    let hit = |p: &str| p == d || p.starts_with(&format!("{d}/"));
    if hit(&p) {
        return true;
    }
    // 脱掉盘符再试一次（`c:/windows/...` → `/windows/...`）
    match strip_drive(&p) {
        Some(rest) => hit(rest),
        None => false,
    }
}

/// 脱掉 `X:` 盘符前缀，返回其后的部分（必须仍以 `/` 开头才算数）。
fn strip_drive(p: &str) -> Option<&str> {
    let b = p.as_bytes();
    if b.len() >= 3 && b[1] == b':' && (b[0] as char).is_ascii_alphabetic() && b[2] == b'/' {
        Some(&p[2..])
    } else {
        None
    }
}

/// 传输路径分级（§4.3「文件传输同等受分级」）：按**本地目的路径**判。
///
/// `sftp_get` 把远端内容落到本地是信任边界跨越；`sftp_put` 反过来也一样受管，
/// 只是它的「目的」在远端——调用方须传对应的那一侧路径。
pub fn classify_local_path(path: &str) -> Tier {
    classify_path(path).write_tier()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_collapses_the_usual_bypasses() {
        for p in [
            "/etc/passwd",
            "/etc//passwd",
            "/etc/./passwd",
            "/etc/../etc/passwd",
            "//etc/passwd",
            "/etc/foo/../passwd",
            "/./etc/passwd",
            "\\etc\\passwd",
        ] {
            assert_eq!(normalize(p), "/etc/passwd", "{p} 未归一化到同一路径");
        }
    }

    #[test]
    fn dotdot_at_root_stays_at_root() {
        assert_eq!(normalize("/.."), "/");
        assert_eq!(normalize("/../.."), "/");
        assert_eq!(normalize("/../etc/passwd"), "/etc/passwd");
    }

    #[test]
    fn relative_dotdot_is_preserved_not_silently_dropped() {
        // 消不掉的 `..` 必须留着：丢掉它会让 `../../etc/passwd` 看起来像 `etc/passwd`
        assert_eq!(normalize("../etc/passwd"), "../etc/passwd");
        assert_eq!(normalize("a/../../etc"), "../etc");
    }

    #[test]
    fn home_forms_are_all_the_same_place() {
        for p in [
            "~/.ssh/authorized_keys",
            "$HOME/.ssh/authorized_keys",
            "${HOME}/.ssh/authorized_keys",
        ] {
            assert_eq!(classify_path(p), PathClass::CodeExecution, "{p}");
        }
        // Windows 形式
        assert_eq!(
            classify_path("%USERPROFILE%\\.ssh\\authorized_keys"),
            PathClass::CodeExecution
        );
    }

    #[test]
    fn home_traversal_into_ssh_is_still_caught() {
        assert_eq!(
            classify_path("~/.ssh/../.ssh/authorized_keys"),
            PathClass::CodeExecution
        );
        assert_eq!(
            classify_path("~/foo/../.ssh/id_rsa"),
            PathClass::CodeExecution
        );
    }

    #[test]
    fn shell_rc_files_are_code_execution() {
        for f in [".bashrc", ".zshrc", ".profile", ".bash_profile", ".zshenv"] {
            assert_eq!(
                classify_path(&format!("~/{f}")),
                PathClass::CodeExecution,
                "~/{f}"
            );
        }
    }

    #[test]
    fn an_rc_named_file_inside_a_project_is_not_code_execution() {
        // 反向对照：`~/proj/.bashrc` 是项目里的一份样例，写它不会获得执行。
        // 判成 dangerous 会让用户对无害操作点确认，久了就会关掉总开关。
        assert_eq!(classify_path("~/proj/.bashrc"), PathClass::Ordinary);
        assert_eq!(classify_path("./dotfiles/.zshrc"), PathClass::Ordinary);
    }

    #[test]
    fn autostart_and_service_locations_are_code_execution() {
        for p in [
            "~/.config/autostart/x.desktop",
            "~/.config/systemd/user/x.service",
            "~/Library/LaunchAgents/x.plist",
            "/Library/LaunchDaemons/x.plist",
            "/etc/systemd/system/x.service",
            "/etc/cron.d/x",
            "/etc/profile.d/x.sh",
            "/var/spool/cron/crontabs/root",
            "~/AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup/x.lnk",
            "~/Documents/WindowsPowerShell/Microsoft.PowerShell_profile.ps1",
        ] {
            assert_eq!(classify_path(p), PathClass::CodeExecution, "{p}");
        }
    }

    #[test]
    fn system_locations_are_system_critical() {
        for p in [
            "/",
            "/etc",
            "/etc/passwd",
            "/boot/vmlinuz",
            "/dev/sda",
            "/proc/1",
            "/usr/bin/ls",
        ] {
            assert_eq!(classify_path(p), PathClass::SystemCritical, "{p}");
        }
    }

    #[test]
    fn segment_matching_avoids_the_prefix_false_positive() {
        // `/etc` 的裸前缀匹配会把这些也算进去
        assert_eq!(classify_path("/etcetera/file"), PathClass::Ordinary);
        assert_eq!(classify_path("/devops/file"), PathClass::Ordinary);
        assert_eq!(classify_path("/bind/file"), PathClass::Ordinary);
    }

    #[test]
    fn ordinary_paths_stay_ordinary() {
        for p in [
            "/tmp/x",
            "~/Downloads/a.txt",
            "./build/out",
            "/var/log/app.log",
            "relative",
        ] {
            assert_eq!(classify_path(p), PathClass::Ordinary, "{p}");
        }
    }

    #[test]
    fn write_tier_maps_the_two_sensitive_classes_to_dangerous() {
        assert_eq!(PathClass::Ordinary.write_tier(), Tier::Write);
        assert_eq!(PathClass::SystemCritical.write_tier(), Tier::Dangerous);
        assert_eq!(PathClass::CodeExecution.write_tier(), Tier::Dangerous);
        // 传输路径走同一套判据（§4.3 信任边界跨越）
        assert_eq!(classify_local_path("~/.bashrc"), Tier::Dangerous);
        assert_eq!(classify_local_path("/tmp/ok"), Tier::Write);
    }

    #[test]
    fn case_insensitive_because_two_of_three_platforms_are() {
        assert_eq!(classify_path("/ETC/passwd"), PathClass::SystemCritical);
        assert_eq!(
            classify_path("C:\\WINDOWS\\system32\\x"),
            PathClass::SystemCritical
        );
    }

    #[test]
    fn windows_drive_prefix_survives_normalization() {
        assert_eq!(normalize("C:\\Windows\\..\\Windows\\x"), "C:/Windows/x");
        assert_eq!(
            classify_path("C:/Windows/System32/drivers/etc/hosts"),
            PathClass::SystemCritical
        );
    }
}
