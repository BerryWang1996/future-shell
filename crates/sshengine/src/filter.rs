//! 排除过滤器（M4a，Xftp「过滤器」对标）：按名字模式隐藏条目 / 跳过传输。
//!
//! 为什么放在引擎层而不是前端：同一套模式要同时管**列表呈现**与（后续的）**目录递归
//! 传输**。两处各写一份匹配器，迟早在某个模式上分叉——而分叉的表现是「界面上看不见的
//! 文件被传上去了」或反过来「以为传了其实跳过了」，两者都不会报错。
//!
//! 语法刻意小：`;` 或换行分隔的若干模式，每条支持 `*`（任意多字符，不跨 `/`）与 `?`
//! （恰一个字符）；以 `/` 结尾的模式只匹配目录。不支持 `**`、字符类 `[a-z]`、否定 `!`
//! ——不是懒，而是**说得清**比覆盖广更重要：这个框里输入的东西决定了哪些文件不会被传，
//! 用户必须能在心里准确预测它的行为。需要更强表达力时应新增一个显式的「高级模式」，
//! 而不是让 `*` 的语义悄悄变得跨目录。

/// 单条模式。`dir_only` 对应以 `/` 结尾的写法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    raw: String,
    dir_only: bool,
}

/// 一组排除模式。空集合永不排除（`excludes` 恒 false）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExcludeFilter {
    patterns: Vec<Pattern>,
}

/// 单个过滤器串的模式条数上限。防的是「把一整个 .gitignore 粘进来」——每条模式都要
/// 对每个条目跑一次匹配，20 000 条目 × 无上限模式数是可以卡死 UI 的。
pub const MAX_PATTERNS: usize = 64;
/// 单条模式的字符上限（同上，且超长模式基本是误粘贴）。
pub const MAX_PATTERN_CHARS: usize = 256;

impl ExcludeFilter {
    /// 解析用户输入。空白模式与超限部分**静默丢弃**（不是错误：这是个随手编辑的输入框，
    /// 把 `*.tmp;;` 判成语法错误只会挡住正常使用）；超出上限的模式被丢弃，条数由
    /// [`Self::len`] 如实反映，调用方据此提示。
    pub fn parse(input: &str) -> Self {
        let mut patterns = Vec::new();
        for raw in input.split([';', '\n', '\r']) {
            let t = raw.trim();
            if t.is_empty() || t.chars().count() > MAX_PATTERN_CHARS {
                continue;
            }
            if patterns.len() >= MAX_PATTERNS {
                break;
            }
            let dir_only = t.ends_with('/');
            let body = t.trim_end_matches('/');
            if body.is_empty() {
                continue; // 单独一个 "/"：不是「排除根目录」，是笔误
            }
            patterns.push(Pattern {
                raw: body.to_string(),
                dir_only,
            });
        }
        Self { patterns }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    /// 这个条目是否被排除。`name` 是**条目名**（不含路径）——模式匹配名字，
    /// 不匹配整条路径：`*.log` 应当在任何目录里都排除 `.log`，而不是只在根下。
    pub fn excludes(&self, name: &str, is_dir: bool) -> bool {
        self.patterns
            .iter()
            .any(|p| (!p.dir_only || is_dir) && wildcard_match(&p.raw, name))
    }
}

/// `*`/`?` 通配匹配（迭代实现，带回溯记忆点）。
///
/// 不用递归：模式与名字都来自外部（服务端可控的文件名、用户可控的模式），
/// `a*a*a*a*…` 这类输入在朴素递归实现上是指数级的——那是一个可以从远端触发的
/// CPU 挂死。这里的回溯版本对每个 `*` 只记一个恢复点，最坏 O(len(pattern) × len(name))。
///
/// 大小写：**区分**。远端主体是 POSIX，`Makefile` 与 `makefile` 是两个文件；
/// 在这里做不区分会让 Windows 用户写的 `*.TMP` 意外命中 `x.tmp`，反过来也一样地错。
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None; // (模式里 * 的下标, 当时的名字下标)
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1; // 先假设 * 吃空串
        } else if let Some((sp, sn)) = star {
            // 回溯：让上一个 * 多吃一个字符
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    // 名字用尽：模式剩下的必须全是 *
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_basics() {
        assert!(wildcard_match("*.tmp", "a.tmp"));
        assert!(wildcard_match("*.tmp", ".tmp")); // * 可吃空串
        assert!(!wildcard_match("*.tmp", "a.tmpx"));
        assert!(wildcard_match("?.log", "a.log"));
        assert!(!wildcard_match("?.log", "ab.log"));
        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "exactly"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("*", "")); // 空名字（不该出现，但不许 panic）
        assert!(!wildcard_match("", "x"));
        assert!(wildcard_match("", ""));
    }

    #[test]
    fn wildcard_multiple_stars_and_backtracking() {
        assert!(wildcard_match("a*b*c", "axxbyyc"));
        assert!(!wildcard_match("a*b*c", "axxbyy"));
        assert!(wildcard_match("*a*a*a", "aaa"));
        assert!(wildcard_match("*.tar.*", "x.tar.gz"));
        assert!(!wildcard_match("*.tar.*", "x.tar"));
        // 病态输入不得指数爆炸（朴素递归实现在这一条上会挂死；这里必须瞬间返回）
        let pat = "a*a*a*a*a*a*a*a*a*a*b";
        let name = "a".repeat(64);
        assert!(!wildcard_match(pat, &name));
    }

    #[test]
    fn wildcard_is_case_sensitive() {
        assert!(!wildcard_match("*.TMP", "a.tmp"));
        assert!(!wildcard_match("makefile", "Makefile"));
    }

    #[test]
    fn parse_splits_and_trims() {
        let f = ExcludeFilter::parse(" *.tmp ; *.log\n node_modules/ ");
        assert_eq!(f.len(), 3);
        assert!(f.excludes("x.tmp", false));
        assert!(f.excludes("x.log", false));
        assert!(f.excludes("node_modules", true));
        // dir_only 模式不匹配同名文件
        assert!(!f.excludes("node_modules", false));
        assert!(!f.excludes("x.txt", false));
    }

    #[test]
    fn parse_tolerates_junk_but_keeps_meaning() {
        // 空段、纯分隔符、单独的 "/" 都丢掉，不算语法错误
        let f = ExcludeFilter::parse(";;  ;\n/;*.o");
        assert_eq!(f.len(), 1, "只该留下 *.o");
        assert!(f.excludes("a.o", false));
        // 空过滤器永不排除
        let empty = ExcludeFilter::parse("   \n ; ");
        assert!(empty.is_empty());
        assert!(!empty.excludes("anything", false));
        assert!(!empty.excludes("anything", true));
    }

    #[test]
    fn parse_caps_pattern_count_and_length() {
        let many = (0..(MAX_PATTERNS + 10))
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(";");
        assert_eq!(ExcludeFilter::parse(&many).len(), MAX_PATTERNS);
        // 超长单条被丢弃，正常条目仍在
        let long = "x".repeat(MAX_PATTERN_CHARS + 1);
        let f = ExcludeFilter::parse(&format!("{long};*.keep"));
        assert_eq!(f.len(), 1);
        assert!(f.excludes("a.keep", false));
    }

    #[test]
    fn matches_by_name_not_by_path() {
        // 模式匹配条目名：`*.log` 在任何深度都该命中，而不是只在根下
        let f = ExcludeFilter::parse("*.log");
        assert!(f.excludes("app.log", false));
        // 名字里含 `/` 不该被当成路径拆分（POSIX 下 `/` 不可能出现在单个条目名里，
        // 但服务端可以谎报——不许因此漏过或误伤）
        assert!(!f.excludes("dir/app.txt", false));
        assert!(
            f.excludes("dir/app.log", false),
            "* 不跨 / 的语义只对模式生效，名字整体参与匹配"
        );
    }
}
