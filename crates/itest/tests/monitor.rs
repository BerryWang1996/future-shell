//! 监控采集与主机状态灯的容器集成测试（M4a 出口标准两条）。
//!
//! 出口标准原文：
//!   · 监控采集：「Linux 容器 + macOS 真机两路验证，数值与同时刻 `top`/`vm_stat` 读数
//!     误差 ≤5%；采集失败静默降级（日志 warn，UI 不弹错误框）」——**容器一路**在此，
//!     macOS 真机一路只能由用户在真机上核验（本仓无 macOS 运行环境，不假装跑过）。
//!   · 主机状态灯轮询：「主机停机/恢复后灯态在 2 个轮询周期内翻转；轮询失败静默降级；
//!     itest/单测覆盖停机检测、恢复检测、连续失败降级三用例」。
//!
//! 为什么误差判据必须在容器里做：解析器单测喂的是我手写的 `/proc/meminfo` 样例，
//! 它只能证明「解析器读得懂我写的格式」。真机上要证的是另一件事——我们采集的数字与
//! 系统工具（free/top）**同时刻**读到的是同一件事实，而不是把 available 当成 free、
//! 或把 KiB 当成 MiB（两者都会让解析器单测全绿而界面上的数字错一个量级）。

use fs_itest::sshd::SshdContainer;
use fs_sshengine::monitor::{monitor_command, parse_cpu_line, parse_monitor_output};

/// 监控数值与容器内 `free`/`nproc` 的同时刻读数一致（内存误差 ≤5%）。
#[tokio::test(flavor = "multi_thread")]
async fn monitor_numbers_match_container_tools_within_5_percent() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("monitor").await.unwrap();

    // 被测命令与对照命令在**同一次 exec** 里跑，中间只隔几毫秒——分两次 exec 会让
    // 「误差」里混进真实的内存波动，那时 5% 这个阈值就不再是在衡量我们的采集正确性。
    //
    // 负载在采集**之前和之后**各读一次：内核每 5 s 刷新 /proc/loadavg，几毫秒的间隔仍可能
    // 恰好跨过一次刷新（CI 上撞过：我们 5.01、对照 4.93）。5 s 一次的刷新不可能在这几毫秒里
    // 发生两次，所以前后两次读数里至少有一次与采集时刻相同。
    let combined = format!(
        "FS_REF_L0=$(cat /proc/loadavg)\n{cmd}\necho '===REF==='\nfree -m | awk '/^Mem:/{{print \"REF_TOTAL=\" $2; print \"REF_USED=\" $3}}'\necho \"REF_LOAD0=$FS_REF_L0\"\necho \"REF_LOAD=$(cat /proc/loadavg)\"\nhead -1 /proc/stat",
        cmd = monitor_command()
    );
    let out = sshd.run(&combined).await.unwrap();
    let (ours_raw, reference) = out
        .split_once("===REF===")
        .unwrap_or_else(|| panic!("对照段分隔符不见了；实得：{out}"));

    let snap = parse_monitor_output(ours_raw);
    let mut ref_total = 0u64;
    let mut ref_used = 0u64;
    for line in reference.lines() {
        if let Some(v) = line.strip_prefix("REF_TOTAL=") {
            ref_total = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("REF_USED=") {
            ref_used = v.trim().parse().unwrap_or(0);
        }
    }
    assert!(
        ref_total > 0,
        "对照读数取不到（free -m 输出变了？）：{reference}"
    );

    let total = snap
        .mem_total_mb
        .unwrap_or_else(|| panic!("采集到的内存总量为空，快照={snap:?}"));
    let used = snap
        .mem_used_mb
        .unwrap_or_else(|| panic!("采集到的内存用量为空，快照={snap:?}"));

    // 总量：应当**完全**相等（同一时刻的同一个常量，差一点就说明单位或字段取错了）
    assert_eq!(
        total, ref_total,
        "内存总量与 free -m 不一致：我们 {total} MiB vs free {ref_total} MiB\
         （差一个量级 = KiB/MiB 混淆；差几倍 = 取错字段）"
    );
    // 用量：容器里进程极少，波动很小；5% 是出口标准给的阈值，另加 8 MiB 绝对宽容
    // 处理「total 很小时 5% 只有 1–2 MiB」的情形——那时一次 shell fork 就能超差。
    let diff = used.abs_diff(ref_used);
    let tol = std::cmp::max((ref_used as f64 * 0.05) as u64, 8);
    assert!(
        diff <= tol,
        "内存用量与 free -m 差 {diff} MiB（我们 {used} / free {ref_used}），超出容差 {tol} MiB"
    );

    // 主机名/运行时长：非空即证明这两条命令的输出被真的接上了（空串是降级路径，
    // 在一台正常容器上出现就说明命令串写错了）
    assert!(!snap.hostname.is_empty(), "主机名为空，快照={snap:?}");
    assert!(!snap.uptime.is_empty(), "运行时长为空，快照={snap:?}");
    assert!(
        snap.load_1.is_some(),
        "负载为空（Linux 容器上不该降级），快照={snap:?}"
    );
    // 三项必须分别对上 /proc/loadavg 的第 1/2/3 列。只断言 load_1 非空太松：
    // 三项都取同一列（5/15 分钟恒等于 1 分钟）会活下来，而那在界面上看不出来。
    // 判据是「三项**整体**等于采集前或采集后的某一次读数」：逐列放宽容差的话，负载接近时
    // 取错列也能混过去；整体相等则不会。
    let ref_load = |key: &str| -> Vec<Option<f32>> {
        reference
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap_or_else(|| panic!("对照负载 {key} 取不到：{reference}"))
            .split_whitespace()
            .take(3)
            .map(|x| x.parse().ok())
            .collect()
    };
    let (before, after) = (ref_load("REF_LOAD0="), ref_load("REF_LOAD="));
    assert_eq!(before.len(), 3, "对照负载不是三项：{before:?}");
    assert_eq!(after.len(), 3, "对照负载不是三项：{after:?}");
    let ours = vec![snap.load_1, snap.load_5, snap.load_15];
    assert!(
        ours == before || ours == after,
        "负载三项与 /proc/loadavg 对不上——取错列（我们 {ours:?}，采集前 {before:?}，采集后 {after:?}）"
    );

    // CPU：单次采集只有累计值，百分比要两次差量。这里验「拿得到 /proc/stat 首行且能解析」，
    // 差量算法本身由单测 cpu_percent_between 钉。
    let stat_line = reference
        .lines()
        .find(|l| l.starts_with("cpu "))
        .unwrap_or_else(|| panic!("/proc/stat 首行取不到：{reference}"));
    assert!(
        parse_cpu_line(stat_line).is_some(),
        "真实 /proc/stat 首行解析失败：{stat_line:?}"
    );
    assert!(
        snap.cpu_times.is_some(),
        "采集快照里没有 CPU 累计值（Linux 容器上不该缺），快照={snap:?}"
    );
}

/// 采集失败**静默降级**：命令缺失时字段为空，而不是整条报错。
///
/// 判据取自出口标准的后半句「采集失败静默降级（日志 warn，UI 不弹错误框）」。
/// 在容器里把 `free`/`uptime`/`df` 从 PATH 上摘掉（不删文件，改用一个空 PATH 前缀目录），
/// 采集应当仍然返回一个快照——只是相应字段为空。
#[tokio::test(flavor = "multi_thread")]
async fn monitor_degrades_silently_when_tools_are_missing() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("mondegrade").await.unwrap();
    // 用一个只含 sh/echo/cat 的 PATH 跑同一条命令：free/uptime/df/nproc 全部找不到
    let out = sshd
        .run(&format!(
            "mkdir -p /tmp/emptybin && ln -sf /bin/sh /tmp/emptybin/sh 2>/dev/null; \
             PATH=/tmp/emptybin sh -c {cmd}",
            cmd = shell_quote(monitor_command())
        ))
        .await
        .expect("即便工具缺失，命令本身也必须成功退出（每一项都有 || echo '' 兜底）");
    let snap = parse_monitor_output(&out);
    // 关键：整体没有失败，只是字段空——UI 渲染「—」。逐项断言而不是「任一为空即过」：
    // 后者太松，实现把其余三项写成整条失败也照旧全绿。
    assert!(
        snap.mem_total_mb.is_none(),
        "free 缺失时内存总量应为空，快照={snap:?}"
    );
    assert!(
        snap.mem_used_mb.is_none(),
        "free 缺失时内存用量应为空，快照={snap:?}"
    );
    assert!(
        snap.disk_total.is_empty(),
        "df 缺失时磁盘总量应为空串，快照={snap:?}"
    );
    assert!(
        snap.load_1.is_none(),
        "awk/loadavg 取不到时负载应为空，快照={snap:?}"
    );
    assert!(
        snap.cpu_times.is_none(),
        "head 缺失时 CPU 累计值应为空，快照={snap:?}"
    );
    // 键完整性：兜底的意义不只是「某项为空」，而是**整条命令跑到底**、每个 KEY 都还在。
    // 少了这条，「第一项失败就中止」的实现照旧全绿——而那会让后面所有字段一起消失，
    // 前端拿到的是半个快照而非降级快照。
    for key in [
        "HOSTNAME=",
        "UPTIME=",
        "LOAD1=",
        "LOAD5=",
        "LOAD15=",
        "MEM_USED=",
        "MEM_TOTAL=",
        "DISK_USED=",
        "DISK_TOTAL=",
        "CPU=",
    ] {
        assert!(
            out.contains(key),
            "降级输出缺少键 {key:?}——命令在中途中止了（兜底不是「某项为空」，是「跑到底」）。完整输出：{out:?}"
        );
    }
    // `CPU=` 恒在这条契约曾被 macOS 分支破过一次：第一版写成
    // 「有 /proc 就打 CPU=，否则打 CPU_PCT=」，降级时 `CPU=` 整个消失，上面那个
    // 循环当场转红。修法是让 `CPU=` 无条件输出、`CPU_PCT=` 只在它为空时追加。
    //
    // 这里再钉一次那个「只在该出现时出现」：**降级时 CPU_PCT 该在**（没有 /proc），
    // 而下一条 Linux 用例里它**不该在**（有 /proc，不该白花一秒去跑 top）。
    // 两个方向都有断言，「无条件打两行」这种偷懒改法过不去。
    assert!(
        out.contains("CPU_PCT="),
        "没有 /proc 时应当追加 CPU_PCT 行（哪怕值为空）。完整输出：{out:?}"
    );
}

/// 状态灯：停机检测 / 恢复检测 / 连续失败降级三用例。
///
/// 判据是**真实 TCP 探测**对着一台真会停会起的机器。
///
/// 「停机」必须停**容器**，不能只停容器里的 sshd 服务——实测：Docker 的端口发布代理
/// （docker-proxy / Desktop 的 vpnkit）在容器内服务已停时**仍然接受** TCP 连接，
/// 于是探测照旧成功、这条测试假红（第一版就撞在这上面）。这也解释了产品侧的一个边界：
/// 只做 TCP 握手的状态灯在「容器还在但服务死了」这一形态下会亮绿；那是 Xshell 同款
/// 语义（见 monitor_cmd::host_probe 的注释），不是本测试要改的事。
///
/// `2 个轮询周期内翻转` 换算成这里的判据是「停机后探测必须失败、恢复后必须成功」；
/// 轮询节奏本身是前端定时器的事（由 settings 单测钉），不在这一层重复。
#[tokio::test(flavor = "multi_thread")]
async fn host_probe_detects_down_recovery_and_stays_degraded() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("hostprobe").await.unwrap();
    let addr = sshd.addr().await;

    // ① 正常：可达
    assert!(
        probe(addr).await,
        "sshd 正常时探测必须成功（否则后两问都没意义）"
    );

    // ② 停机：整台容器停掉后必须探测失败
    sshd.container.stop_with_timeout(Some(5)).await.unwrap();
    let down = wait_until(addr, false).await;
    assert!(down, "主机停机后探测仍成功——状态灯会一直亮绿，掩盖真实故障");

    // ③ 连续失败保持失败（不得自己「恢复」成绿：那会让红灯闪一下就没了）
    for i in 0..3 {
        assert!(
            !probe(addr).await,
            "第 {} 次连续探测报成功，而服务仍是停的",
            i + 1
        );
    }

    // ④ 恢复：机器起回来后必须重新变绿。
    //    注意端口映射会**换号**（Docker 重启容器后重新分配），故重新取一次地址——
    //    拿旧地址去探测只会证明「旧端口没人听」，与恢复检测无关。
    sshd.container.start().await.unwrap();
    let new_addr = sshd.addr().await;
    let up = wait_until(new_addr, true).await;
    assert!(
        up,
        "主机恢复后探测仍失败（新地址 {new_addr}）——恢复了却一直红灯，用户会去查一台好机器"
    );
}

/// 与生产 `probe_tcp` 同形：只做 TCP 握手、不读 banner、超时=不可达。
///
/// 这里重写一份而不是调用 app 的命令：那是 `#[tauri::command]`，itest 拿不到 State。
/// 判据形状（握手成功即可达）是产品语义，重写的这份必须与它一致——不一致就等于在测别的东西。
async fn probe(addr: std::net::SocketAddr) -> bool {
    let timeout = std::time::Duration::from_millis(1500);
    tokio::task::spawn_blocking(move || {
        std::net::TcpStream::connect_timeout(&addr, timeout).is_ok()
    })
    .await
    .unwrap_or(false)
}

/// 轮询等探测结果翻转到 `want`。上限 30s：s6 停/起服务在慢机上要几秒。
async fn wait_until(addr: std::net::SocketAddr, want: bool) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        if probe(addr).await == want {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    false
}

/// 把一段 shell 命令包成单引号字面量（内部单引号按 '\'' 逃逸）。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ═══════════ macOS 采集分支：在 Linux 上用 shim 把它逼出来跑 ═══════════
//
// 2026-08-26 之前，`monitor_command()` 采的全是 Linux 专有源（`/proc/loadavg`、
// `free -m`、`/proc/stat`），macOS 上 6 项指标里**负载三项、内存两项、CPU 一项恒空**。
// 那不是「待核验」，是**未实现**。现已补上 macOS 分支（`sysctl -n vm.loadavg` /
// `vm_stat` + `hw.pagesize`/`hw.memsize` / `top -l 2`）。
//
// 本仓没有 macOS 机器，但这**不等于这条分支只能靠肉眼审**。分支里真正会写错的
// 是 shell 与 awk：字段序号数错一位、`tr -d '{}'` 漏掉一边、页数忘了乘页大小、
// 单位少除 1048576——这些错误全都可以在 Linux 上暴露出来，只要：
//   ① 把 Linux 那些源藏掉（PATH 里没有 `free`/`nproc`，`/proc` 用一个空目录 bind 掉）；
//   ② 在 PATH 上放 `sysctl` / `vm_stat` / `top` 三个 shim，吐**真 macOS 格式**的输出。
//
// 剩下未被覆盖的只有一个问题：「真 macOS 的 `vm_stat` 是不是真的这么打印」。
// 那是一个有文档、格式几十年没变的问题，比「这段 awk 对不对」小得多。
// 这条测试把风险从后者缩到前者——但**不代表 macOS 真机已验**，路线图照记。

/// 三个 shim 的内容。数字是选过的：
/// - 页大小 4096、总内存 16 GiB；
/// - active 1,000,000 + wired 200,000 + compressed 100,000 = 1,300,000 页
///   → 1,300,000 × 4096 / 1048576 = **5078 MiB**（整除，便于精确断言）；
/// - 负载 1.85/1.94/2.01；CPU user 4.16% + sys 8.34% = **12.5%**。
///
/// `vm_stat` 的字段位置是这条测试真正在守的东西：
///   `Pages active:            N.`        → $3
///   `Pages wired down:        N.`        → $4
///   `Pages occupied by compressor: N.`   → $5
/// 三行的词数不同，数错一位就取到别的东西。故 shim 里**故意混进** free/inactive/
/// speculative 等其它行——只留三行的话，awk 就算写成 `$NF` 也能过。
const MACOS_SHIMS: &str = r#"
mkdir -p /tmp/macshim /tmp/noproc
cat > /tmp/macshim/sysctl <<'EOF'
#!/bin/sh
# 只认 `sysctl -n <key>`
case "$2" in
  vm.loadavg) echo '{ 1.85 1.94 2.01 }' ;;
  hw.pagesize) echo 4096 ;;
  hw.memsize) echo 17179869184 ;;
  hw.ncpu) echo 10 ;;
  *) exit 1 ;;
esac
EOF
cat > /tmp/macshim/vm_stat <<'EOF'
#!/bin/sh
echo 'Mach Virtual Memory Statistics: (page size of 4096 bytes)'
echo 'Pages free:                               50000.'
echo 'Pages active:                           1000000.'
echo 'Pages inactive:                          300000.'
echo 'Pages speculative:                        20000.'
echo 'Pages throttled:                              0.'
echo 'Pages wired down:                        200000.'
echo 'Pages purgeable:                          10000.'
echo 'Pages occupied by compressor:            100000.'
EOF
cat > /tmp/macshim/top <<'EOF'
#!/bin/sh
# **必须认 `-l N`**。第一版的 shim 无视参数、恒打两份样本，于是断言钉住的是
# 「我那段 awk 后覆盖前」，而不是「产品用的是 -l 2」——变异「把 -l 2 改成 -l 1」
# 照样绿。真 top 的 -l 决定采样份数，shim 不照做就测不出那个分水岭。
n=1
while [ $# -gt 0 ]; do
  case "$1" in -l) n="$2"; shift 2 ;; *) shift ;; esac
done
i=1
while [ "$i" -le "$n" ]; do
  echo 'Processes: 500 total, 2 running, 498 sleeping'
  if [ "$i" = 1 ]; then
    # 第一份 = 开机以来的平均（真 top 就是这样）
    echo 'CPU usage: 2.00% user, 3.00% sys, 95.00% idle'
  else
    echo 'CPU usage: 4.16% user, 8.34% sys, 87.50% idle'
  fi
  i=$((i+1))
done
EOF
cat > /tmp/macshim/hostname <<'EOF'
#!/bin/sh
echo mac-test
EOF
cat > /tmp/macshim/uptime <<'EOF'
#!/bin/sh
echo '14:03  up 3 days,  2:15, 3 users, load averages: 1.85 1.94 2.01'
EOF
chmod +x /tmp/macshim/*
for b in sh awk df cat head grep tr sed printf; do
  p=$(command -v "$b" 2>/dev/null) && ln -sf "$p" /tmp/macshim/"$b" 2>/dev/null
done
# `/tmp/noproc`：两个**只挡 /proc** 的 cat/head。
#
# 第一版用 `unshare -m` + `mount --bind` 把 /proc 整个挂空——容器不是特权容器，
# `unshare(0x20000): Operation not permitted`，而且那条命令让引号嵌套变成三层、
# 内层 sh 直接语法错。改成在**脚本真正触及 /proc 的那两处**下手：
# `cat /proc/loadavg` 与 `head -n 1 /proc/stat`。挡得更准，也不需要任何特权。
cat > /tmp/noproc/cat <<'EOF'
#!/bin/sh
for a in "$@"; do case "$a" in /proc/*) exit 1 ;; esac; done
exec /bin/cat "$@"
EOF
cat > /tmp/noproc/head <<'EOF'
#!/bin/sh
for a in "$@"; do case "$a" in /proc/*) exit 1 ;; esac; done
exec /bin/head "$@"
EOF
chmod +x /tmp/noproc/*
"#;

#[tokio::test(flavor = "multi_thread")]
async fn the_macos_branch_produces_real_numbers_when_linux_sources_are_absent() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("monmac").await.unwrap();
    // PATH 里**没有** $PATH：free/nproc 于是找不到，正如 macOS 上那样。
    // `/tmp/noproc` 排在前面，它的 cat/head 对 `/proc/*` 一律非零退出——
    // 脚本触及 /proc 的只有那两处，挡住它们就等于「这台机器没有 /proc」。
    let out = sshd
        .run(&format!(
            "{shims} PATH=/tmp/noproc:/tmp/macshim sh -c {cmd}",
            shims = MACOS_SHIMS,
            cmd = shell_quote(monitor_command())
        ))
        .await
        .expect("macOS 分支应当跑得完");
    let snap = parse_monitor_output(&out);

    // 负载：`sysctl -n vm.loadavg` → `{ 1.85 1.94 2.01 }`，花括号必须被剥掉。
    // 漏剥一边的话 `1.85` 前面会粘一个 `{`，parse::<f32>() 失败 → None。
    assert_eq!(snap.load_1, Some(1.85), "负载 1 分钟不对，快照={snap:?}");
    assert_eq!(snap.load_5, Some(1.94));
    assert_eq!(snap.load_15, Some(2.01));

    // 内存：(1,000,000 + 200,000 + 100,000) 页 × 4096 B / 1 MiB = 5078 MiB。
    // 这个数把三件事一起钉住：三个字段各自取对了位置、页数乘了页大小、单位除对了。
    assert_eq!(snap.mem_used_mb, Some(5078), "内存用量不对，快照={snap:?}");
    // 16 GiB = 16384 MiB
    assert_eq!(
        snap.mem_total_mb,
        Some(16384),
        "内存总量不对，快照={snap:?}"
    );

    // CPU：取 `top` 的**第二份**样本（4.16 + 8.34 = 12.5），不是第一份（2.00 + 3.00 = 5.0）。
    // 第一份是「开机以来的平均」，在一台开机三十天的机器上与「此刻忙不忙」无关。
    // 这条断言正是用来区分 `-l 1` 与 `-l 2` 的——写成前者会得到 5.0。
    assert_eq!(
        snap.cpu_percent_direct,
        Some(12.5),
        "CPU 百分比不对（取错了 top 的样本？），快照={snap:?}"
    );
    // 且**不该**有累计值——那一路是 Linux 的，两条路混用会让下游拿百分比去做差量。
    assert!(
        snap.cpu_times.is_none(),
        "macOS 分支不该产出 /proc/stat 累计值，快照={snap:?}"
    );

    // 核数走 `sysctl -n hw.ncpu`
    assert_eq!(snap.cpu_cores, Some(10), "核数不对，快照={snap:?}");
    assert_eq!(snap.hostname, "mac-test");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_linux_branch_wins_when_both_sources_are_available() {
    // 反向对照：shim 在 PATH 上、但 `/proc` 也在。此时**必须**走 Linux 那一路。
    //
    // 没有这一条的话，一个「总是走 macOS 分支」的实现会让上一条全绿，而在 Linux 上
    // 白白多花一秒（`top -l 2 -s 1`）并拿到一个精度更差的数。优先级不是风格问题：
    // `/proc/stat` 的差量更准，且不额外耗时。
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("monboth").await.unwrap();
    let out = sshd
        .run(&format!(
            "{shims} PATH=/tmp/macshim:$PATH sh -c {cmd}",
            shims = MACOS_SHIMS,
            cmd = shell_quote(monitor_command())
        ))
        .await
        .expect("命令应当跑得完");
    let snap = parse_monitor_output(&out);
    assert!(
        snap.cpu_times.is_some(),
        "/proc/stat 在的时候必须走 Linux 那一路，快照={snap:?}"
    );
    assert!(
        snap.cpu_percent_direct.is_none(),
        "走了 Linux 那一路就不该再跑 top（白花一秒），快照={snap:?}"
    );
    // 更硬的一条：`CPU_PCT` 这一行**根本不该被打出来**。只看解析后的字段不够——
    // 打了一行空的 `CPU_PCT=` 同样解析成 None，而那意味着 top 已经跑过、
    // 一秒已经花掉了。判据要落在原始输出上。
    assert!(
        !out.contains("CPU_PCT="),
        "有 /proc 时不该输出 CPU_PCT 行（说明 top 被跑了）。完整输出：{out:?}"
    );
    // 负载与内存同理：容器里的真实值，不是 shim 里那三个数。
    assert_ne!(snap.load_1, Some(1.85), "负载取到了 shim 的值——走错分支了");
    assert_ne!(
        snap.mem_total_mb,
        Some(16384),
        "内存总量取到了 shim 的值——走错分支了"
    );
}
