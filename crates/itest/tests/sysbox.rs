//! 系统面工具箱的容器集成测试（M7.1 出口标准一、二）。
//!
//! 出口原文：
//!   · 「服务管理：列表/状态/启停重启/看日志五件事对真容器跑通」
//!   · 「补丁盘点：五种包管理器各有解析单测 + **至少两种**在容器 itest 里对照
//!     （被测与对照同一次 exec），『0 个可更新』与『包管理器不存在』必须是两种结果」
//!
//! # 为什么对照必须在同一次 exec 里
//!
//! 分两次跑的话，两次之间容器的包索引可能变（尤其在 `apk update` 之后），
//! 那时「不一致」到底是我们解析错了还是世界变了，无从分辨。同一次 exec 里先跑我们的命令、
//! 再跑原生命令，中间只隔几毫秒——这条纪律与 `monitor.rs` 的 `===REF===` 同源。
//!
//! # 解析器单测证不了什么
//!
//! `crates/sshengine/src/packages.rs` 的五个解析器喂的是**我手写的**样例，它们只能证明
//! 「解析器读得懂我写的格式」。真机上要证的是另一件事：那五条命令在真发行版上打出来的
//! 到底长什么样。故本文件的断言一律拿**容器现打的输出**作对照，不引入任何手写样例。

use fs_itest::distro::DistroContainer;
use fs_sshengine::packages::{parse_scan, scan_command, PackageManager, PackageScan};
use fs_sshengine::services::{
    action_command, journal_command, list_services_command, parse_service_list, status_command,
    ServiceAction, ServiceListResult,
};

fn skip() -> bool {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return true;
    }
    false
}

/// 把「我们的输出」与「对照输出」在同一段 stdout 里切开。
fn split_ref(out: &str) -> (&str, &str) {
    out.split_once("===REF===")
        .unwrap_or_else(|| panic!("对照段分隔符不见了；实得：{out}"))
}

// ───────────────────────── 补丁盘点：两种真管理器对照 ─────────────────────────

/// Alpine（apk）。用 **3.16**：`latest` 与 `3.18` 实测都是 0 个可升级（镜像随补丁重建），
/// 而一条恒等于 0 的断言证不了解析器读对了任何东西。3.16 已停止重建，同分支内累积的补丁
/// 稳定可见（2026-09-03 实测 musl / busybox / ca-certificates-bundle 等五条）。
#[tokio::test(flavor = "multi_thread")]
async fn apk_updates_match_the_container_tool_in_one_exec() {
    if skip() {
        return;
    }
    let c = DistroContainer::start("alpine", "3.16").await.unwrap();
    c.exec_ok("apk update >/dev/null 2>&1 || true")
        .await
        .unwrap();

    let combined = format!(
        "{ours}\necho '===REF==='\napk version -l '<' 2>/dev/null | tail -n +2",
        ours = scan_command()
    );
    let out = c.exec(&combined).await.unwrap().stdout;
    let (ours_raw, reference) = split_ref(&out);

    let scan = parse_scan(ours_raw);
    let PackageScan::Updates { manager, updates } = scan else {
        panic!(
            "[{}] 应探测到 apk，实得 {scan:?}\n原始输出：{ours_raw}",
            c.label
        );
    };
    assert_eq!(manager, PackageManager::Apk, "[{}] 管理器认错了", c.label);

    let ref_lines: Vec<&str> = reference
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.contains('<'))
        .collect();
    assert!(
        !ref_lines.is_empty(),
        "[{}] 对照命令一条可升级都没给出——本用例的前提（旧分支必有补丁）不成立了，\
         请换一个更旧的 tag 或检查网络。对照原文：{reference}",
        c.label
    );
    assert_eq!(
        updates.len(),
        ref_lines.len(),
        "[{}] 条目数与对照不一致\n我们的：{updates:#?}\n对照：{reference}",
        c.label
    );
    // 逐个包名都要能在对照原文里找到——数目对上但名字全错的解析器同样会通过计数断言
    for u in &updates {
        assert!(
            reference.contains(&u.name),
            "[{}] 解析出的包名 {:?} 在对照输出里根本没出现\n对照：{reference}",
            c.label,
            u.name
        );
        assert!(
            !u.candidate.is_empty(),
            "[{}] {:?} 的目标版本为空",
            c.label,
            u.name
        );
    }
}

/// Debian（apt）。
///
/// bullseye-slim 的镜像是**带着安全补丁重建**的，直接查是 0 个可升级——同样证不了什么。
/// 故先用 `-t bullseye` 装基础套件（而非 security 套件）的 curl：apt 的默认发行版被指到
/// bullseye 之后会优先取那个较旧的版本，装完 security 里的新版就成了「可升级」。
/// 这不是伪造数据——包、版本、索引全是真的，只是把机器摆成一个必然有补丁待打的真实状态
/// （2026-09-03 实测稳定得到 curl / libcurl4 / libnghttp2-14 三条）。
#[tokio::test(flavor = "multi_thread")]
async fn apt_updates_match_the_container_tool_in_one_exec() {
    if skip() {
        return;
    }
    let c = DistroContainer::start("debian", "bullseye-slim")
        .await
        .unwrap();
    c.exec_ok("apt-get update >/dev/null 2>&1 || true")
        .await
        .unwrap();
    c.exec_ok(
        "apt-get install -y -t bullseye --no-install-recommends curl >/dev/null 2>&1 || true",
    )
    .await
    .unwrap();

    let combined = format!(
        "{ours}\necho '===REF==='\napt list --upgradable 2>/dev/null | grep -c 'upgradable from'",
        ours = scan_command()
    );
    let out = c.exec(&combined).await.unwrap().stdout;
    let (ours_raw, reference) = split_ref(&out);

    let scan = parse_scan(ours_raw);
    let PackageScan::Updates { manager, updates } = scan else {
        panic!(
            "[{}] 应探测到 apt，实得 {scan:?}\n原始输出：{ours_raw}",
            c.label
        );
    };
    assert_eq!(manager, PackageManager::Apt, "[{}] 管理器认错了", c.label);

    let ref_count: usize = reference
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("[{}] 对照计数读不出（{e}）：{reference}", c.label));
    assert!(
        ref_count > 0,
        "[{}] 对照命令一条可升级都没给出——本用例的前提（oldstable 必有补丁）不成立了",
        c.label
    );
    assert_eq!(
        updates.len(),
        ref_count,
        "[{}] 条目数与对照不一致\n我们的：{updates:#?}",
        c.label
    );
    for u in &updates {
        assert!(
            !u.name.is_empty() && !u.name.contains('/'),
            "[{}] 包名没剥干净：{u:?}",
            c.label
        );
        assert!(
            u.current.is_some(),
            "[{}] apt 给了 upgradable from，却没解析出当前版本：{u:?}",
            c.label
        );
    }
}

/// 出口原文点名的那一条：「包管理器不存在」与「0 个可更新」必须是两种结果。
///
/// 造一台**真的没有任何已知包管理器**的机器：把五个可执行文件从 PATH 上挪走。
/// 这比找一个天生没有包管理器的镜像更可控，且证的是同一件事——探测分支走到了 `MGR=none`。
#[tokio::test(flavor = "multi_thread")]
async fn missing_package_manager_is_not_zero_updates() {
    if skip() {
        return;
    }
    let c = DistroContainer::start("alpine", "3.20").await.unwrap();

    // 先证「有 apk 时是 Updates 变体」，再证「藏起来之后变成 NoManager」——
    // 少了前半截，后半截可能只是因为命令整体没跑起来。
    let before = parse_scan(&c.exec(scan_command()).await.unwrap().stdout);
    assert!(
        matches!(
            before,
            PackageScan::Updates {
                manager: PackageManager::Apk,
                ..
            }
        ),
        "[{}] 藏起来之前就该探测到 apk，实得 {before:?}",
        c.label
    );

    c.exec_ok("for b in apt dnf zypper apk pacman; do p=$(command -v $b 2>/dev/null) && mv \"$p\" \"$p.hidden\"; done; true")
        .await
        .unwrap();

    let after = parse_scan(&c.exec(scan_command()).await.unwrap().stdout);
    assert_eq!(
        after,
        PackageScan::NoManager,
        "[{}] 没有任何包管理器时必须是 NoManager，而不是「0 个可更新」",
        c.label
    );
}

// ───────────────────────── 服务管理：真 systemd 容器 ─────────────────────────

/// 非 systemd 的真机器上，服务列表必须是 `NoSystemd` 而不是空列表。
///
/// 这条在 Alpine 上跑（它用 OpenRC，天生没有 systemctl）——**不是**造出来的场景，
/// 而是相当一部分真实容器与嵌入式机器的常态。
#[tokio::test(flavor = "multi_thread")]
async fn alpine_has_no_systemd_and_says_so() {
    if skip() {
        return;
    }
    let c = DistroContainer::start("alpine", "3.20").await.unwrap();
    let out = c.exec(list_services_command()).await.unwrap().stdout;
    assert_eq!(
        parse_service_list(&out),
        ServiceListResult::NoSystemd,
        "[{}] 没有 systemctl 的机器必须报 NoSystemd；原始输出：{out}",
        c.label
    );
}

/// 五件事对真 systemd 跑通：列表 / 状态 / 日志 / 停 / 启（重启在最后收尾）。
///
/// 用**真 systemd 作 PID 1**（privileged + host cgroupns）。拿一个没 boot systemd 的容器
/// 是证不了这条的：那时 `systemctl` 只会回「System has not been booted with systemd」，
/// 走的是失败分支，而出口要的是列表与动作真的生效。
///
/// # 为什么另立一个环境变量，以及本条**未跑过**
///
/// 本机 Docker Desktop（WSL2 后端）**起不来 systemd 作 PID 1**：2026-09-03 实测四组参数组合
/// （`--privileged` / `+--cgroupns=host` / `+--cgroupns=private` / `+挂 /sys/fs/cgroup`）
/// 容器一律 `Exited (255)` 且无任何日志。这是宿主环境的限制，不是本仓代码的问题。
///
/// 故本条门控在 `FS_ITEST_SYSTEMD=1` 之后，需要一台**能跑 systemd 容器的 Docker 主机**
/// （Linux 原生 Docker，或 CI 的 Linux runner）。它与其余用例共用 `FS_ITEST`——多这一道
/// 门是为了不让「本机跑不了」变成一条恒红的用例，但**绝不**改成静默跳过后当作通过：
/// 路线图里这一条标 `[!]`，我没跑过，不代签。
///
/// 解析那一半另有真实数据兜底：`crates/sshengine/tests/services_real_output.rs` 用的是从
/// 本机 WSL2 的**真 systemd**（`is-system-running` = running）原样抓下来的 103 行输出。
/// 那份夹具证得了「列切得对」，证不了「动作真的生效」——后者只有本条能证。
#[tokio::test(flavor = "multi_thread")]
async fn systemd_container_covers_list_status_journal_and_actions() {
    if skip() {
        return;
    }
    if std::env::var("FS_ITEST_SYSTEMD").is_err() {
        eprintln!(
            "skip: 需要能跑 systemd 容器的 Docker 主机；设 FS_ITEST_SYSTEMD=1 启用。\n\
             本机 Docker Desktop(WSL2) 实测起不来 systemd 作 PID 1（见本用例文档注）。"
        );
        return;
    }
    let c = DistroContainer::start_systemd("jrei/systemd-ubuntu", "22.04")
        .await
        .unwrap();
    let boot = c
        .exec("systemctl is-system-running 2>&1 || true")
        .await
        .unwrap();
    assert!(
        matches!(
            boot.stdout.trim(),
            "running" | "degraded" | "starting" | "maintenance"
        ),
        "[{}] systemd 没接管容器，后面的断言全都无从谈起：{}",
        c.label,
        boot.stdout
    );

    // ① 列表：必须是 Units 变体且非空（真 systemd 至少有若干基础单元）
    let list = c.exec(list_services_command()).await.unwrap().stdout;
    let ServiceListResult::Units { units } = parse_service_list(&list) else {
        panic!(
            "[{}] 真 systemd 上必须解析成 Units；原始输出：{list}",
            c.label
        );
    };
    assert!(
        !units.is_empty(),
        "[{}] 真 systemd 上一个单元都没解析出来；原始输出：{list}",
        c.label
    );
    // 逐字段都要有内容——半个单元在界面上是一行没有名字的东西
    for u in &units {
        assert!(
            u.name.ends_with(".service"),
            "[{}] 单元名不像服务：{u:?}",
            c.label
        );
        assert!(
            !u.load.is_empty() && !u.active.is_empty() && !u.sub.is_empty(),
            "[{}] 字段空：{u:?}",
            c.label
        );
    }

    // 挑一个真实存在、且可以随便停的单元来做动作。cron 在这个镜像里有；
    // 取不到就退回列表里第一个 active 的（除了 systemd 自己那几个关键单元）。
    let target = "cron.service".to_string();
    let has_cron = units.iter().any(|u| u.name == target);
    let target = if has_cron {
        target
    } else {
        units
            .iter()
            .find(|u| {
                u.active == "active" && !u.name.starts_with("systemd-") && u.name != "dbus.service"
            })
            .map(|u| u.name.clone())
            .unwrap_or_else(|| panic!("[{}] 找不到可安全操作的单元；列表：{units:#?}", c.label))
    };

    // ② 状态：拿得到原文，且提到了单元名
    let st = c
        .exec(&status_command(&target).unwrap())
        .await
        .unwrap()
        .stdout;
    assert!(
        st.contains(&target),
        "[{}] status 输出里没有单元名：{st}",
        c.label
    );

    // ③ 日志：命令跑得通（内容可能为空——容器里的服务不一定写过日志，那是正常的）
    let jr = c
        .exec(&journal_command(&target, 20).unwrap())
        .await
        .unwrap();
    assert_eq!(
        jr.code,
        Some(0),
        "[{}] journalctl 没跑起来：{}",
        c.label,
        jr.stderr
    );

    // ④ 停：动作生效，列表里那一行的 active 真的变了
    let stop = c
        .exec(&action_command(ServiceAction::Stop, &target).unwrap())
        .await
        .unwrap();
    assert_eq!(
        stop.code,
        Some(0),
        "[{}] stop 失败：{}",
        c.label,
        stop.stderr
    );
    let after_stop = c.exec(list_services_command()).await.unwrap().stdout;
    let ServiceListResult::Units { units: u2 } = parse_service_list(&after_stop) else {
        panic!("[{}] stop 之后列表解析不出来", c.label)
    };
    let row = u2.iter().find(|u| u.name == target).unwrap_or_else(|| {
        panic!(
            "[{}] stop 之后 {target} 从列表里消失了（--all 应当仍列出它）",
            c.label
        )
    });
    assert_ne!(
        row.active, "active",
        "[{}] stop 之后仍是 active：{row:?}",
        c.label
    );

    // ⑤ 启 + 重启：都要成功，且最终回到 active——不收尾的话这个容器留下一个停掉的服务，
    //    虽然容器马上就销毁了，但「动作真的双向生效」正是这一条要证的。
    let start = c
        .exec(&action_command(ServiceAction::Start, &target).unwrap())
        .await
        .unwrap();
    assert_eq!(
        start.code,
        Some(0),
        "[{}] start 失败：{}",
        c.label,
        start.stderr
    );
    let restart = c
        .exec(&action_command(ServiceAction::Restart, &target).unwrap())
        .await
        .unwrap();
    assert_eq!(
        restart.code,
        Some(0),
        "[{}] restart 失败：{}",
        c.label,
        restart.stderr
    );

    let final_list = c.exec(list_services_command()).await.unwrap().stdout;
    let ServiceListResult::Units { units: u3 } = parse_service_list(&final_list) else {
        panic!("[{}] 收尾列表解析不出来", c.label)
    };
    let row = u3.iter().find(|u| u.name == target).unwrap();
    assert_eq!(
        row.active, "active",
        "[{}] restart 之后没回到 active：{row:?}",
        c.label
    );
}

/// 单元名校验是安全边界，容器里再证一次：**注入串根本到不了远端**。
///
/// 单测已经证了 `validate_unit_name` 拒得对；这里证的是另一件事——命令构造器把它拒在了
/// 拼串之前，所以不存在「构造出一条带分号的命令、只是碰巧没人执行」的中间状态。
#[tokio::test(flavor = "multi_thread")]
async fn injection_never_becomes_a_command() {
    if skip() {
        return;
    }
    for evil in [
        "nginx; touch /tmp/pwned",
        "a$(touch /tmp/pwned2)",
        "a`touch /tmp/pwned3`",
    ] {
        assert!(status_command(evil).is_err(), "{evil:?} 竟构造出了状态命令");
        assert!(
            journal_command(evil, 10).is_err(),
            "{evil:?} 竟构造出了日志命令"
        );
        assert!(
            action_command(ServiceAction::Restart, evil).is_err(),
            "{evil:?} 竟构造出了动作命令"
        );
    }
}
