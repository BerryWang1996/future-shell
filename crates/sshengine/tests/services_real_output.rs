//! `parse_service_list` 对**真实 systemd 输出**的解析（M7.1）。
//!
//! # 为什么单独一个文件、为什么用夹具
//!
//! `src/services.rs` 里的单测喂的是我手写的四行样例——它们只能证明「解析器读得懂我写的格式」。
//! 真机上要证的是另一件事：`systemctl list-units --type=service --all --plain --no-legend --no-pager`
//! 在真发行版上打出来的到底长什么样（列宽多少、`not-found`/`masked` 这些 load 态怎么排、
//! 描述里有没有制表符、失败单元行首有没有装饰字符）。
//!
//! 夹具 `fixtures/systemctl-list-units-ubuntu2204.txt` 是 2026-09-03 从一台**真跑着 systemd**
//! 的 Ubuntu 22.04（本机 WSL2，`systemctl is-system-running` = running）原样抓下来的 103 行，
//! 一个字符没改。它不是手写样例。
//!
//! # 它替代不了什么（如实记）
//!
//! 夹具证的是**解析**，证不了**动作**（start/stop/restart 真的生效）。那一半要一台能跑 systemd
//! 的 Docker 主机——本机 Docker Desktop（WSL2 后端）起不来 systemd 作 PID 1，四组参数组合
//! （privileged / cgroupns=host / cgroupns=private / 挂 cgroup）实测全部 `Exited (255)`。
//! 动作那一半见 `crates/itest/tests/sysbox.rs` 里那条需 `FS_ITEST_SYSTEMD=1` 的用例，**未跑过**。

use fs_sshengine::services::{parse_service_list, ServiceListResult};

const REAL: &str = include_str!("fixtures/systemctl-list-units-ubuntu2204.txt");

/// 命令首行的 `SYSTEMD=1` 由 `list_services_command` 的 shell 打出，夹具里只有表本身，
/// 故在这里补上——补的是**我们自己的**标记，不是被测数据。
fn with_mark(body: &str) -> String {
    format!("SYSTEMD=1\n{body}")
}

#[test]
fn every_line_of_a_real_table_becomes_a_unit() {
    let ServiceListResult::Units { units } = parse_service_list(&with_mark(REAL)) else {
        panic!("真实输出必须解析成 Units");
    };
    let data_lines = REAL.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        units.len(),
        data_lines,
        "真实表里有 {data_lines} 行，只解析出 {} 个单元——有行被静默丢了",
        units.len()
    );
    assert!(units.len() >= 50, "夹具太小，证不了什么：{}", units.len());
}

#[test]
fn no_field_is_empty_and_names_look_like_units() {
    let ServiceListResult::Units { units } = parse_service_list(&with_mark(REAL)) else {
        panic!()
    };
    for u in &units {
        assert!(
            u.name.ends_with(".service"),
            "名字不像服务单元（列切错了？）：{u:?}"
        );
        assert!(!u.load.is_empty(), "load 为空：{u:?}");
        assert!(!u.active.is_empty(), "active 为空：{u:?}");
        assert!(!u.sub.is_empty(), "sub 为空：{u:?}");
        // 描述可以为空吗？真实表里 not-found 单元的描述就是单元名本身，从不为空。
        assert!(!u.description.is_empty(), "描述为空：{u:?}");
    }
}

/// 真实表里的 load 态不止 `loaded`——`not-found` 与 `masked` 都在。
/// 把它们错切进别的列会让 active/sub 整体右移一位，而每个字段仍然「非空」，
/// 上一条断言抓不到；这条按**取值域**抓。
#[test]
fn load_active_sub_columns_are_not_shifted() {
    let ServiceListResult::Units { units } = parse_service_list(&with_mark(REAL)) else {
        panic!()
    };
    const LOADS: &[&str] = &["loaded", "not-found", "masked", "bad-setting", "error"];
    const ACTIVES: &[&str] = &[
        "active",
        "inactive",
        "failed",
        "activating",
        "deactivating",
        "reloading",
    ];
    for u in &units {
        assert!(
            LOADS.contains(&u.load.as_str()),
            "load 列不是已知取值：{u:?}"
        );
        assert!(
            ACTIVES.contains(&u.active.as_str()),
            "active 列不是已知取值（列错位了？）：{u:?}"
        );
    }
    // 非空证明：三种 load 态在夹具里都出现过，否则上面那条只覆盖了 loaded 一种
    for want in ["loaded", "not-found", "masked"] {
        assert!(
            units.iter().any(|u| u.load == want),
            "夹具里没有 load={want} 的行，这条断言覆盖不到它"
        );
    }
    for want in ["active", "inactive"] {
        assert!(units.iter().any(|u| u.active == want), "夹具里没有 {want}");
    }
}

/// 描述里的空格必须原样保留：切碎了的话界面上是一串断词。
#[test]
fn descriptions_keep_their_words() {
    let ServiceListResult::Units { units } = parse_service_list(&with_mark(REAL)) else {
        panic!()
    };
    let cron = units
        .iter()
        .find(|u| u.name == "cron.service")
        .expect("夹具里应有 cron.service");
    assert_eq!(
        cron.description, "Regular background program processing daemon",
        "描述被切碎或截断了"
    );
    assert_eq!(cron.active, "active");
    assert_eq!(cron.sub, "running");
}

/// 模板实例单元（`getty@tty1.service`）的 `@` 必须过得了名字校验——
/// 拒掉它等于让一大类真实单元在界面上点不动。
#[test]
fn template_instance_units_survive_name_validation() {
    let ServiceListResult::Units { units } = parse_service_list(&with_mark(REAL)) else {
        panic!()
    };
    let templated: Vec<_> = units.iter().filter(|u| u.name.contains('@')).collect();
    assert!(
        !templated.is_empty(),
        "夹具里没有模板实例单元，这条断言覆盖不到它"
    );
    for u in templated {
        assert_eq!(
            fs_sshengine::services::validate_unit_name(&u.name),
            Ok(()),
            "真实存在的单元 {:?} 被名字校验拒了",
            u.name
        );
    }
}

/// 反向：夹具里**每一个**真实单元名都必须能过校验。校验太严会让服务管理对真机不可用，
/// 而那种「太严」不会被注入用例抓到（那些只证太松的方向）。
#[test]
fn no_real_unit_name_is_rejected_by_the_whitelist() {
    let ServiceListResult::Units { units } = parse_service_list(&with_mark(REAL)) else {
        panic!()
    };
    let rejected: Vec<_> = units
        .iter()
        .filter(|u| fs_sshengine::services::validate_unit_name(&u.name).is_err())
        .map(|u| u.name.clone())
        .collect();
    assert!(
        rejected.is_empty(),
        "白名单把真实存在的单元拒了（太严 = 这些服务在界面上点不动）：{rejected:?}"
    );
}
