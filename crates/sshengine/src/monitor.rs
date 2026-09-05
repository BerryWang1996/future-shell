//! 系统监控（M4a）：一次 exec 采集主机名 / 运行时长 / 负载 / 内存 / 根分区磁盘 / CPU%。
//!
//! 设计取舍：
//! - **单次 exec、一条命令、`KEY=value` 逐行输出**：命令串是纯函数、解析器是纯函数，
//!   两者都能离线测试；不引入任何新依赖，也不在引擎里发第二、第三条命令。
//! - **CPU% 走两次 `/proc/stat` 采样的差量**（M4a 补齐）：同一时刻的累计值没有意义，
//!   调用方（app 层）按会话存上次采样、与本次差量出百分比（[`cpu_percent_between`]）。
//!   首轮没有前值 ⇒ 空态「—」，第二轮起才有数。
//! - **单脚本双路，由远端 shell 自己选**（2026-08-26）：Linux 走 `/proc/loadavg` /
//!   `free -m` / `/proc/stat`；取不到时落到 macOS/BSD 一路——`sysctl -n vm.loadavg`
//!   （输出形如 `{ 1.85 1.94 2.01 }`，花括号要剥）、`vm_stat` 页数 × `hw.pagesize`
//!   配 `hw.memsize`、核数 `hw.ncpu`、CPU 走 `top -l 2` 并取**第二份**样本。
//!
//!   做成一个脚本而不是两个：两个脚本意味着 app 得先知道远端是什么系统才能问，
//!   而「远端是什么系统」本身要问一次——鸡生蛋。
//!
//!   CPU 那一路另开 [`MonitorSnapshot::cpu_percent_direct`] 而不复用 `cpu_times`，
//!   理由见该字段文档（累计值与百分比是两种形状，混用会让差量拿两个百分比相减）。
//!
//!   **2026-08-22 之前这里只写「非 Linux 时 CPU 行为空」，说轻了**——当时内存与负载
//!   同样为空，6 项里 5 项恒空，那是「未实现」不是「降级」。现已实现，
//!   但**真 macOS 上一次也没跑过**：验证靠 `crates/itest/tests/monitor.rs` 里
//!   用 shim 在 Linux 上把这条分支逼出来跑（能覆盖 shell 与 awk 的全部写法错误，
//!   覆盖不了「真 vm_stat 是不是真这么打印」）。
//! - 空态一律经前端渲染「—」，不为降级单开错误面。
//! - **一切 `2>/dev/null || echo ''` 兜底**：目标机缺 `uptime`/`free`/`df`/`/proc` 时
//!   单项为空，而不是整条命令失败。字段因此都是 `Option`/空串，前端按缺省渲染「—」。

use serde::Serialize;

/// 一次采集的快照。字段为 `Option`：单项采集失败（命令缺失/权限）时为空，整条命令失败
/// 才整体 Err。磁盘用量保持人类可读串（`df -h` 原样），不转数值——不同单位（K/M/G/T）互转
/// 徒增误判面，展示本来就要这个串。
#[derive(Debug, Clone, Default, Serialize)]
pub struct MonitorSnapshot {
    pub hostname: String,
    pub uptime: String,
    pub load_1: Option<f32>,
    pub load_5: Option<f32>,
    pub load_15: Option<f32>,
    pub mem_used_mb: Option<u64>,
    pub mem_total_mb: Option<u64>,
    pub disk_used: String,
    pub disk_total: String,
    /// CPU 占用率（0..=100，一次小数）。由调用方对**先后两次**采样差量得出；
    /// 解析器只透传原始 `/proc/stat` cpu 行（[`CpuTimes`]）。
    pub cpu_percent: Option<f64>,
    /// 本次采样的原始 cpu 行（差量计算的输入；不序列化给前端——它只消费百分比）。
    #[serde(skip)]
    pub cpu_times: Option<CpuTimes>,
    /// 远端**直接给出**的 CPU 百分比（macOS 的 `top -l 2` 那一路）。
    ///
    /// 为什么要有第二条路：`/proc/stat` 给的是开机以来的累计 jiffies，靠两次采样
    /// 求差量；macOS 没有等价的累计计数器可从 shell 读到，`top` 给的直接就是
    /// 一个区间百分比。两者形状不同，**硬塞进 `cpu_times` 是错的**——那会让
    /// 差量计算拿两个百分比去相减。
    ///
    /// 调用方的优先级：有 `cpu_times` 就走差量（Linux，更准且不额外耗时），
    /// 否则用这个值。两者都无 ⇒ `cpu_percent` 留空（宁缺毋假）。
    ///
    /// **代价**：`top -l 2 -s 1` 要等一秒。`-l 1` 不行——它的第一份样本是
    /// 「开机以来的平均」，那个数在一台开机三十天的机器上永远接近某个中值，
    /// 与「此刻忙不忙」无关。宁可慢一秒也不要一个稳定的错数。
    #[serde(skip)]
    pub cpu_percent_direct: Option<f64>,
    /// CPU 核数（`nproc`，回落 `/proc/cpuinfo` 的 processor 行数）。
    ///
    /// M4b「监控 × AI 联动」加的。加它的理由很具体：**负载数字离开核数就没有意义**——
    /// `load_1 = 5.0` 在单核机上意味着五倍过载、在 64 核机上是几乎空闲。
    /// 没有这一项，前端的异常判定就只能对负载放弃判断（或者拍一个必然错的阈值），
    /// 而负载恰好是运维最先看的那个数。
    ///
    /// `None` = 采不到（非 Linux、或两条命令都不可用）。那时前端**不判**负载，
    /// 而不是假设一个核数——猜错方向会让「一切正常」和「已经过载」互相调换。
    pub cpu_cores: Option<u32>,
}

/// `/proc/stat` 首行 `cpu  user nice system idle iowait irq softirq steal ...` 的
/// 各列累计值（单位：jiffies，单调递增）。未知尾列忽略——内核加新计数器不该让
/// 旧客户端的解析失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuTimes {
    /// 总累计（全部列之和）。
    pub fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }

    /// 非空闲累计（busy）——idle/iowait 之外都是「在干活」；steal 是被宿主偷走的时间，
    /// 对虚拟机而言也是真实占用，计入 busy。
    pub fn busy(&self) -> u64 {
        self.user + self.nice + self.system + self.irq + self.softirq + self.steal
    }
}

/// 解析 `cpu  u n s i w irq sirq steal ...` 行（`/proc/stat` 首行）。形状不符返回 `None`
/// （缺列到 idle 为止按 0 补——极老内核至少有前四列）。
pub fn parse_cpu_line(line: &str) -> Option<CpuTimes> {
    let rest = line.strip_prefix("cpu")?;
    // "cpu  123 ..."：前缀后至少一个空白；紧贴的 "cpu0"（per-core 行）不匹配——
    // strip_prefix("cpu") 后首字符非空白即说明是 per-core 行，返回 None。
    let rest = rest.strip_prefix(' ')?;
    let cols: Vec<u64> = rest
        .split_whitespace()
        .map(|c| c.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let g = |i: usize| cols.get(i).copied().unwrap_or(0);
    Some(CpuTimes {
        user: g(0),
        nice: g(1),
        system: g(2),
        idle: g(3),
        iowait: g(4),
        irq: g(5),
        softirq: g(6),
        steal: g(7),
    })
}

/// 两次采样间的 CPU 百分比（S310）：`(busyΔ / totalΔ) × 100`，钳到 0..=100。
/// 无前值、总差量为 0（两次采样同一 jiffy，间隔过短）或时钟倒走（totalΔ 为负
/// 不可能于 u64 饱和减法下出现，防御性钳位）都返回 `None`——**宁缺毋假**：
/// 一个编造的 0% 或 87% 比空态「—」误导得多。
pub fn cpu_percent_between(prev: &CpuTimes, cur: &CpuTimes) -> Option<f64> {
    let total_d = cur.total().checked_sub(prev.total())?;
    let busy_d = cur.busy().checked_sub(prev.busy())?;
    if total_d == 0 {
        return None;
    }
    let pct = (busy_d as f64 / total_d as f64) * 100.0;
    Some(pct.clamp(0.0, 100.0))
}

/// 采集命令。多行即多条命令，目标机登录 shell 逐条执行；每行输出一个 `KEY=value`。
/// 只读、幂等、不改远程任何状态。
///
/// # `CPU=` 无条件输出，`CPU_PCT=` 只在没有 `/proc/stat` 时才追加
///
/// 这个不对称是**被一条既有守卫逼出来的**。降级用例
/// （`itest/tests/monitor.rs::monitor_degrades_silently_when_tools_are_missing`）
/// 钉的是「整条命令跑到底、每个 KEY 都还在」——半个快照与降级快照是两回事。
/// macOS 分支第一版写成 `if 有 /proc { echo CPU= } else { echo CPU_PCT= }`，
/// 于是降级时 `CPU=` 这个键消失了，那条守卫当场转红。**该改的是脚本不是守卫**。
///
/// 现在：`CPU=` 恒在（无 `/proc` 时为空串），`CPU_PCT=` 只在 `CPU=` 为空时才多打一行。
/// 于是 Linux 上**不会**去跑 `top -l 2`（那要等一秒），而键的集合仍是稳定的。
pub fn monitor_command() -> &'static str {
    r#"
echo "HOSTNAME=$(hostname 2>/dev/null || echo '')"
echo "UPTIME=$(uptime -p 2>/dev/null || uptime 2>/dev/null || echo '')"
L=$(cat /proc/loadavg 2>/dev/null)
if [ -z "$L" ]; then L=$(sysctl -n vm.loadavg 2>/dev/null | tr -d '{}'); fi
if [ -z "$L" ]; then L='   '; fi
echo "LOAD1=$(echo "$L" | awk '{print $1}')"
echo "LOAD5=$(echo "$L" | awk '{print $2}')"
echo "LOAD15=$(echo "$L" | awk '{print $3}')"
M=$(free -m 2>/dev/null | awk '/Mem:/{print $3" "$2}')
if [ -z "$M" ]; then
PS=$(sysctl -n hw.pagesize 2>/dev/null)
TOT=$(sysctl -n hw.memsize 2>/dev/null)
M=$(vm_stat 2>/dev/null | awk -v ps="$PS" -v tot="$TOT" '
/Pages active/{a=$3} /Pages wired down/{w=$4} /Pages occupied by compressor/{c=$5}
END{gsub(/\./,"",a);gsub(/\./,"",w);gsub(/\./,"",c);
if(ps>0&&tot>0)printf "%d %d",(a+w+c)*ps/1048576,tot/1048576}')
fi
echo "MEM_USED=$(echo "$M" | awk '{print $1}')"
echo "MEM_TOTAL=$(echo "$M" | awk '{print $2}')"
D=$(df -h / 2>/dev/null | awk 'NR==2{print $3" "$2}')
echo "DISK_USED=$(echo "$D" | awk '{print $1}')"
echo "DISK_TOTAL=$(echo "$D" | awk '{print $2}')"
C=$(head -n 1 /proc/stat 2>/dev/null)
echo "CPU=$C"
if [ -z "$C" ]; then
echo "CPU_PCT=$(top -l 2 -n 0 -s 1 2>/dev/null | awk '/CPU usage/{u=$3;s=$5} END{gsub(/%/,"",u);gsub(/%/,"",s);if(u!="")printf "%.1f",u+s}')"
fi
CORES=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '')
echo "CORES=$CORES"
"#
}

/// 把 [`monitor_command`] 的 stdout 解析成快照。逐行 `KEY=value`；空值/未知键/缺失键一律
/// 落成对应字段的空态——**绝不 panic**（命令在远端跑，输出形状不可尽信）。
pub fn parse_monitor_output(stdout: &str) -> MonitorSnapshot {
    let mut s = MonitorSnapshot::default();
    for line in stdout.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "HOSTNAME" => s.hostname = v.to_string(),
            "UPTIME" => s.uptime = v.to_string(),
            "LOAD1" => s.load_1 = v.parse().ok(),
            "LOAD5" => s.load_5 = v.parse().ok(),
            "LOAD15" => s.load_15 = v.parse().ok(),
            "MEM_USED" => s.mem_used_mb = v.parse().ok(),
            "MEM_TOTAL" => s.mem_total_mb = v.parse().ok(),
            "DISK_USED" => s.disk_used = v.to_string(),
            "DISK_TOTAL" => s.disk_total = v.to_string(),
            "CPU" => s.cpu_times = parse_cpu_line(v),
            // 钳到 0..=100 而不是原样透传：远端给的是一个我们无法核对的数，
            // 而 `120%` 上屏之后没人能分辨是「机器真的这样」还是「解析错了」。
            "CPU_PCT" => s.cpu_percent_direct = v.parse::<f64>().ok().map(|p| p.clamp(0.0, 100.0)),
            // 0 核当采不到处理：`grep -c` 那条回落在极端情形下会给出 "0"，
            // 而 0 核会让下游的「负载 / 核数」变成除零。
            "CORES" => s.cpu_cores = v.parse::<u32>().ok().filter(|n| *n > 0),
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_output() {
        let out = "\
HOSTNAME=web-01
UPTIME=up 3 days, 2 hours
LOAD1=0.15
LOAD5=0.20
LOAD15=0.18
MEM_USED=1234
MEM_TOTAL=7984
DISK_USED=12G
DISK_TOTAL=50G
";
        let s = parse_monitor_output(out);
        assert_eq!(s.hostname, "web-01");
        assert_eq!(s.uptime, "up 3 days, 2 hours");
        assert_eq!(s.load_1, Some(0.15));
        assert_eq!(s.load_15, Some(0.18));
        assert_eq!(s.mem_used_mb, Some(1234));
        assert_eq!(s.mem_total_mb, Some(7984));
        assert_eq!(s.disk_used, "12G");
        assert_eq!(s.disk_total, "50G");
    }

    /// 远端缺 free/df/proc 时，对应字段为空态而非整条失败。
    #[test]
    fn missing_commands_yield_empty_fields() {
        let out = "HOSTNAME=web-01\nUPTIME=\n";
        let s = parse_monitor_output(out);
        assert_eq!(s.hostname, "web-01");
        assert_eq!(s.uptime, "");
        assert!(s.load_1.is_none());
        assert!(s.mem_used_mb.is_none());
        assert_eq!(s.disk_used, "");
    }

    /// 非数字的负载/内存值不得把整条解析打崩（远端输出形状不可尽信）。
    #[test]
    fn garbage_values_are_ignored_not_panicking() {
        let s = parse_monitor_output("LOAD1=abc\nMEM_USED=12G\nLOAD5=1.5\n");
        assert_eq!(s.load_1, None);
        assert_eq!(s.mem_used_mb, None); // "12G" 不是纯数字
        assert_eq!(s.load_5, Some(1.5));
    }

    /// 命令产出的键与解析器读的键**双向**一致。
    ///
    /// 这条原先是一张**手写清单**，只断言「清单里的每个键命令都产出了」。
    /// 那样它守不住任何东西：新增一个键时清单里没有它，测试照样绿——
    /// 本次加 `CORES` 就是这么过去的，而漏掉任一侧的后果是
    /// 「远端老老实实算了一个值、前端永远收不到」或者反过来
    /// 「解析器等着一个没人产出的键，那个字段永远是空的」。
    /// 两种都不会有任何报错，只是某个指标一直显示「—」。
    ///
    /// 现在两侧都**从源码里提**：命令侧扫 `echo "KEY=`，解析侧扫本文件里
    /// `match` 的字符串分支。手写清单一旦消失，「忘了同步」就不再是可能的。
    #[test]
    fn command_emits_the_keys_the_parser_reads() {
        let cmd = monitor_command();
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/monitor.rs"),
        )
        .expect("读不到本文件源码");

        // 命令侧：`echo "KEY=`
        let mut emitted: Vec<String> = Vec::new();
        for part in cmd.split("echo \"").skip(1) {
            if let Some((k, _)) = part.split_once('=') {
                // 数字也要认：键名里有 LOAD1/LOAD5/LOAD15。第一版漏了这一条，
                // 两侧**对称地**都少提 3 个键，于是相等断言照样绿——
                // 是上面那两条下限断言把它逮住的。这就是做空防护存在的理由。
                if !k.is_empty()
                    && k.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                {
                    emitted.push(k.to_string());
                }
            }
        }
        emitted.sort();
        emitted.dedup();

        // 解析侧：`parse_monitor_output` 里 `"KEY" => ` 的分支。
        // 只取那个函数的范围，否则本文件里别处的字符串会混进来
        //（包括本测试自己写的那些——那就成了让注释给自己作证）。
        let body_start = src
            .find("pub fn parse_monitor_output")
            .expect("找不到解析函数");
        let body = &src[body_start..];
        let body_end = body.find("\n}\n").expect("找不到解析函数的结尾");
        let body = &body[..body_end];
        // 先剥行注释。不剥的话注释里的引号会把 `split('"')` 的奇偶配对整个打乱——
        // 第一版就栽在这儿：`"CORES"` 那个分支上面的注释里写了个 `"0"`，
        // 于是扫查采出了一个叫 `0` 的「键」。
        // 这与「注释给自己作证」是同一个毛病的另一面：那个是注释让断言变松，
        // 这个是注释让断言胡乱变严，而后者会逼下一个人把断言改松。
        let body: String = body
            .lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let body = body.as_str();
        let mut parsed: Vec<String> = Vec::new();
        for part in body.split('"').skip(1).step_by(2) {
            if !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                parsed.push(part.to_string());
            }
        }
        parsed.sort();
        parsed.dedup();

        // 做空防护：两侧都不能是空的，否则下面的相等断言恒真。
        assert!(
            emitted.len() >= 10,
            "命令侧只提到 {} 个键，扫查坏了：{emitted:?}",
            emitted.len()
        );
        assert!(
            parsed.len() >= 10,
            "解析侧只提到 {} 个键，扫查坏了：{parsed:?}",
            parsed.len()
        );
        assert_eq!(
            emitted, parsed,
            "命令产出的键与解析器读的键不一致。\n\
             命令产出：{emitted:?}\n解析读取：{parsed:?}\n\
             只在命令侧 = 远端白算一个值、前端永远收不到；\n\
             只在解析侧 = 那个字段永远是空的。两种都不报错，只是某个指标一直显示「—」。"
        );
    }

    /// S310：`/proc/stat` cpu 行解析——标准形状、per-core 行不匹配、缺列按 0 补、
    /// 非数字列整行拒绝（宁缺毋假）。
    #[test]
    fn parse_cpu_line_shapes() {
        let t = parse_cpu_line("cpu  100 20 30 400 50 10 5 15").unwrap();
        assert_eq!(
            t,
            CpuTimes {
                user: 100,
                nice: 20,
                system: 30,
                idle: 400,
                iowait: 50,
                irq: 10,
                softirq: 5,
                steal: 15
            }
        );
        // per-core 行（cpu0）不是聚合行
        assert!(parse_cpu_line("cpu0 1 2 3 4 5 6 7 8").is_none());
        // 极老内核只有四列：缺省列按 0
        let t = parse_cpu_line("cpu  100 20 30 400").unwrap();
        assert_eq!(t.iowait, 0);
        assert_eq!(t.steal, 0);
        // 非数字拒绝
        assert!(parse_cpu_line("cpu  100 x 30 400").is_none());
        // 空串（非 Linux 降级：2>/dev/null 后 echo ''）落 None
        assert!(parse_cpu_line("").is_none());
    }

    /// S310：CPU% 差量——busyΔ/totalΔ；iowait 计入 idle 侧；总差量 0（间隔过短）
    /// 与无前值都返回 None（宁缺毋假）；百分比钳到 0..=100。
    #[test]
    fn cpu_percent_between_math() {
        let prev = CpuTimes {
            user: 100,
            nice: 0,
            system: 50,
            idle: 850,
            ..Default::default()
        };
        // 总 1000、busy 150（15%）→ 下一拍 总 2000、busy 450 ⇒ Δbusy=300/Δtotal=1000 = 30%
        let cur = CpuTimes {
            user: 250,
            nice: 0,
            system: 200,
            idle: 1550,
            ..Default::default()
        };
        let p = cpu_percent_between(&prev, &cur).unwrap();
        assert!((p - 30.0).abs() < 0.001, "busyΔ/totalΔ 应为 30%，得 {p}");

        // iowait 属 idle 侧：idle+iowait 互换不改变百分比
        let a = CpuTimes {
            user: 100,
            nice: 0,
            system: 0,
            idle: 900,
            iowait: 0,
            ..Default::default()
        };
        let b = CpuTimes {
            user: 100,
            nice: 0,
            system: 0,
            idle: 0,
            iowait: 900,
            ..Default::default()
        };
        let one = cpu_percent_between(&a, &b);
        assert_eq!(
            one, None,
            "总差量 0（两次同一 jiffy）必须 None，不是编造的数"
        );

        // 时钟倒走（total 回退）→ checked_sub 出 None，不 panic 不回绕
        let small = CpuTimes {
            user: 1,
            ..Default::default()
        };
        let big = CpuTimes {
            user: 1000,
            idle: 1000,
            ..Default::default()
        };
        assert_eq!(cpu_percent_between(&big, &small), None);

        // 全忙 ⇒ 100%
        let idle0 = CpuTimes {
            user: 10,
            ..Default::default()
        };
        let idle1 = CpuTimes {
            user: 110,
            ..Default::default()
        };
        let p = cpu_percent_between(&idle0, &idle1).unwrap();
        assert!((p - 100.0).abs() < 0.001);
    }

    /// 解析器把 CPU 行透传成 CpuTimes（差量输入），不在此算百分比。
    #[test]
    fn parse_output_carries_cpu_times() {
        let s = parse_monitor_output("HOSTNAME=h\nCPU=cpu  1 2 3 4 5 6 7 8\n");
        assert_eq!(s.cpu_times.map(|t| t.user), Some(1));
        assert_eq!(
            s.cpu_percent, None,
            "解析器不算百分比——差量归调用方（app 层持前值）"
        );
        // 非 Linux：CPU= 空 → 前值该被调用方清掉（monitor_cmd 的 else 分支），此处只验解析为 None
        let s = parse_monitor_output("HOSTNAME=h\nCPU=\n");
        assert!(s.cpu_times.is_none());
    }
}
