//! 提示词与审计记录的脱敏（总设计 §4.2 / M2 出口「audit 表…脱敏：prompt 中的 secret
//! 以占位符替代」）。
//!
//! # 这不是「顺手清理一下」
//!
//! AI 的上下文里天然会有密钥：终端上一屏可能刚 `cat` 过一个 `.env`，Profile 里带着
//! 连接口令，`ssh -i` 的输出里有私钥路径。这些内容会走两条路——
//!
//! 1. **发给模型**（云 provider 意味着离开本机）；
//! 2. **落进 audit 表**（本机留存，但会被导出、被截图、被贴进工单）。
//!
//! 第 2 条是本模块的硬要求（出口原文点名）。第 1 条的正解是**不要把屏幕内容放进上下文**
//! （§4.2 的「允许发送屏幕上下文」开关），脱敏只是那之后的第二道——
//! 它挡得住已知形状的密钥，挡不住「一段看起来像散文的机密」。
//! 所以本模块的注释里不会出现「已脱敏所以可以安全发送」这种说法。
//!
//! # 只做确定性替换，不做启发式猜测
//!
//! 每条规则要么是**已知的确切字符串**（来自 Vault 的那份口令），要么是**结构确定的形状**
//! （PEM 头尾、URL 里的 `user:pass@`、带已知前缀的 token）。
//! 刻意不做「这一串看着像密码」的熵值判断：那类规则的假阳性会把命令本身涂掉
//! （`git commit -m a1b2c3d4e5` 里的 hash 被当成密钥），让审计记录失去可读性——
//! 而一份读不懂的审计记录与没有审计记录的区别不大。

/// 替换后留下的占位符。
///
/// 固定串而不是随机串：审计记录要能被 grep、能被两条记录对比。
/// 带上类别是为了让读记录的人知道「这里原本是什么」——
/// 一律涂成 `***` 的话，事后就分不清「口令」和「整个私钥」了。
pub const PLACEHOLDER_PREFIX: &str = "[[redacted:";
pub const PLACEHOLDER_SUFFIX: &str = "]]";

fn placeholder(kind: &str) -> String {
    format!("{PLACEHOLDER_PREFIX}{kind}{PLACEHOLDER_SUFFIX}")
}

/// 一次脱敏的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    /// 脱敏后的文本。
    pub text: String,
    /// 命中的规则与次数，按规则名排序。供审计行记录「涂了什么」。
    ///
    /// 需要它是因为「脱敏了 0 处」和「脱敏了 3 处」对读记录的人意义完全不同：
    /// 前者说明这条 prompt 本来就干净，后者说明原文里有 3 个密钥——
    /// 而那件事本身可能就是要调查的对象。
    pub hits: Vec<(&'static str, usize)>,
}

impl Redacted {
    /// 总共替换了多少处。
    pub fn total(&self) -> usize {
        self.hits.iter().map(|(_, n)| n).sum()
    }

    /// 有没有任何东西被涂掉。
    pub fn any(&self) -> bool {
        self.total() > 0
    }
}

/// 带已知前缀的 token 形状。
///
/// 只列**前缀本身就足以判定**的那些：`sk-` 之后跟一长串是 OpenAI 密钥，
/// 不会是别的东西。刻意不列 `token`/`key` 这种词——那会把讲解文字也涂掉。
const TOKEN_PREFIXES: &[(&str, &str)] = &[
    ("sk-", "api-key"),
    ("sk_live_", "api-key"),
    ("sk_test_", "api-key"),
    ("rk_live_", "api-key"),
    ("pk_live_", "api-key"),
    ("ghp_", "github-token"),
    ("gho_", "github-token"),
    ("ghu_", "github-token"),
    ("ghs_", "github-token"),
    ("ghr_", "github-token"),
    ("github_pat_", "github-token"),
    ("glpat-", "gitlab-token"),
    ("xoxb-", "slack-token"),
    ("xoxp-", "slack-token"),
    ("xoxa-", "slack-token"),
    ("xapp-", "slack-token"),
    ("AKIA", "aws-access-key"),
    ("ASIA", "aws-access-key"),
    ("AIza", "google-api-key"),
    ("ya29.", "google-oauth-token"),
    ("hf_", "huggingface-token"),
    ("dop_v1_", "digitalocean-token"),
    ("npm_", "npm-token"),
    ("SG.", "sendgrid-key"),
];

/// token 前缀后面至少要跟多少个字符才算命中。
///
/// 挡住的是「文档里提到 `sk-` 这个前缀」这类假阳性。取 12 是因为所有真实密钥都远长于此，
/// 而散文里连续 12 个非空白的 token 字符几乎只能是密钥本身。
const MIN_TOKEN_BODY: usize = 12;

/// PEM 私钥块的头部形状。
///
/// 私钥是最不能漏的一类：它不是「一个可以改的口令」，而是一把可能开很多门的钥匙，
/// 且长度足以撑满整个上下文窗口。
const PEM_BEGINS: &[&str] = &[
    "-----BEGIN OPENSSH PRIVATE KEY-----",
    "-----BEGIN RSA PRIVATE KEY-----",
    "-----BEGIN DSA PRIVATE KEY-----",
    "-----BEGIN EC PRIVATE KEY-----",
    "-----BEGIN PRIVATE KEY-----",
    "-----BEGIN ENCRYPTED PRIVATE KEY-----",
    "-----BEGIN PGP PRIVATE KEY BLOCK-----",
];

/// 名字里带这些词的赋值，其**值**要涂掉（`PASSWORD=hunter2`）。
///
/// 按名字判定而不是按值判定，是这条规则能成立的关键：值长什么样我们不知道，
/// 但 `PASSWORD=` 左边那个词是确定的。
const SECRET_NAME_HINTS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "access_key",
    "secret_key",
    "private_key",
    "credential",
    "auth",
    "bearer",
    "session_key",
    "client_secret",
];

/// 对一段文本做脱敏。
///
/// `known_secrets` 是调用方**确切知道**的密钥（从 Vault 里取出来准备用的那几条）。
/// 它们优先且必须被替换——这是本函数唯一能给出的强保证；其余规则都是形状匹配，
/// 只能说「挡住了已知的那些形状」。
pub fn redact(text: &str, known_secrets: &[&str]) -> Redacted {
    let mut out = text.to_string();
    let mut hits: Vec<(&'static str, usize)> = Vec::new();
    let bump = |name: &'static str, n: usize, hits: &mut Vec<(&'static str, usize)>| {
        if n == 0 {
            return;
        }
        match hits.iter_mut().find(|(k, _)| *k == name) {
            Some((_, c)) => *c += n,
            None => hits.push((name, n)),
        }
    };

    // ① 已知密钥：确切字符串替换。**必须先做**——
    // 若某条已知口令恰好长得像别的形状，先被形状规则涂成占位符，
    // 后面这一步就再也匹配不到它，于是「已知密钥一定被涂掉」这个唯一的强保证就没了。
    // （结果上两者都涂掉了，但保证的来源变成了偶然。）
    for s in known_secrets {
        // 太短的「已知密钥」不替换：一个 1–3 字符的口令会把正常文本打成筛子
        // （口令是 `a` 的话，全文每个 a 都变占位符，审计记录直接不可读）。
        if s.chars().count() < 4 {
            continue;
        }
        let n = out.matches(*s).count();
        if n > 0 {
            out = out.replace(*s, &placeholder("known-secret"));
            bump("known-secret", n, &mut hits);
        }
    }

    // ② PEM 私钥块：整块涂掉，从 BEGIN 到对应的 END
    let (t, n) = redact_pem_blocks(&out);
    out = t;
    bump("private-key", n, &mut hits);

    // ③ URL 里的凭据：`scheme://user:pass@host` → 只涂 `pass`
    let (t, n) = redact_url_credentials(&out);
    out = t;
    bump("url-credential", n, &mut hits);

    // ④ 带已知前缀的 token
    let (t, n) = redact_prefixed_tokens(&out);
    out = t;
    bump("api-token", n, &mut hits);

    // ⑤ `NAME=value` 里名字看着像密钥的
    let (t, n) = redact_named_assignments(&out);
    out = t;
    bump("named-secret", n, &mut hits);

    hits.sort_unstable();
    Redacted { text: out, hits }
}

/// 把 PEM 私钥块整体替换掉。
fn redact_pem_blocks(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut n = 0usize;
    'outer: loop {
        // 找最早出现的那个 BEGIN
        let mut best: Option<(usize, &str)> = None;
        for b in PEM_BEGINS {
            if let Some(at) = rest.find(b) {
                if best.is_none_or(|(cur, _)| at < cur) {
                    best = Some((at, b));
                }
            }
        }
        let Some((at, begin)) = best else {
            break 'outer;
        };
        out.push_str(&rest[..at]);
        out.push_str(&placeholder("private-key"));
        n += 1;
        // 找对应的 END。找不到就说明这块被截断了（终端上一屏刚好切在中间）——
        // 那更要涂：剩下的全是密钥材料。
        let after = &rest[at + begin.len()..];
        let kind = begin
            .trim_start_matches('-')
            .trim_start_matches("BEGIN ")
            .trim_end_matches('-');
        let end_marker = format!("-----END {kind}-----");
        match after.find(&end_marker) {
            Some(e) => rest = &after[e + end_marker.len()..],
            None => match after.find("-----END") {
                // END 标记的类别不匹配（罕见，但不能因此漏掉）：按最近的 END 收尾
                Some(e) => {
                    rest = match after[e..].find("-----\n").or_else(|| after[e..].find('\n')) {
                        Some(nl) => &after[e + nl..],
                        None => "",
                    }
                }
                None => {
                    rest = "";
                }
            },
        }
    }
    out.push_str(rest);
    (out, n)
}

/// `scheme://user:password@host` 里的口令。
///
/// 只涂口令不涂用户名：用户名对读审计的人有用（「是谁的凭据泄露了」），而它不是秘密。
fn redact_url_credentials(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut n = 0usize;
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        // authority 段到第一个 `/ ? # 空白` 为止
        let auth_end = tail
            .find(['/', '?', '#', ' ', '\t', '\n', '"', '\''])
            .unwrap_or(tail.len());
        let (auth, after) = tail.split_at(auth_end);
        match auth.rsplit_once('@') {
            Some((userinfo, host)) => match userinfo.split_once(':') {
                Some((user, pass)) if !pass.is_empty() => {
                    out.push_str(user);
                    out.push(':');
                    out.push_str(&placeholder("url-credential"));
                    out.push('@');
                    out.push_str(host);
                    n += 1;
                }
                _ => out.push_str(auth),
            },
            None => out.push_str(auth),
        }
        rest = after;
    }
    out.push_str(rest);
    (out, n)
}

/// 一个字符能不能出现在 token 里。含 `=` 是因为 base64 的补位。
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | '=')
}

/// 这个字符出现在前缀**之前**时，是否说明前缀并不在词首。
///
/// 与 [`is_token_char`] 的差别只有一个 `=`，但那个差别是必要的：
/// `export K=sk-…` 里 `sk-` 的前一个字符是 `=`，而 `=` 在真实写法里是**赋值符**
/// 而不是 token 的一部分。把它算作「词内字符」会让所有 `KEY=<token>` 形式漏检
/// ——那恰好是密钥在终端上最常见的样子。
fn blocks_word_start(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+')
}

fn redact_prefixed_tokens(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut n = 0usize;
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    'scan: while i < bytes.len() {
        // 前缀必须落在「词首」：`ask-me` 里的 `sk-` 不算
        let at_word_start = i == 0 || !blocks_word_start(bytes[i - 1]);
        if at_word_start {
            for (prefix, kind) in TOKEN_PREFIXES {
                let pc: Vec<char> = prefix.chars().collect();
                if bytes[i..].starts_with(&pc[..]) {
                    let body_start = i + pc.len();
                    let mut j = body_start;
                    while j < bytes.len() && is_token_char(bytes[j]) {
                        j += 1;
                    }
                    if j - body_start >= MIN_TOKEN_BODY {
                        out.push_str(&placeholder(kind));
                        n += 1;
                        i = j;
                        continue 'scan;
                    }
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    (out, n)
}

/// `NAME=value` / `NAME: value` 里名字看着像密钥的，涂掉值。
fn redact_named_assignments(text: &str) -> (String, usize) {
    let mut n = 0usize;
    let mut result = Vec::new();
    for line in text.split_inclusive('\n') {
        let (body, nl) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        let mut out = String::with_capacity(body.len());
        let mut rest = body;
        loop {
            // 找下一个分隔符（`=` 或 `: `）
            let Some(sep_at) = rest.find(['=', ':']) else {
                out.push_str(rest);
                break;
            };
            let sep = rest.as_bytes()[sep_at] as char;
            // `:` 只有后跟空白才算「键: 值」（避免打到 `http://`、`C:\`、时间 `12:30`）
            if sep == ':' && !rest[sep_at + 1..].starts_with([' ', '\t']) {
                out.push_str(&rest[..sep_at + 1]);
                rest = &rest[sep_at + 1..];
                continue;
            }
            // 键 = 分隔符左边那一串「标识符字符」
            let key_start = rest[..sep_at]
                .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                .map(|p| p + 1)
                .unwrap_or(0);
            let key = &rest[key_start..sep_at];
            let key_l = key.to_ascii_lowercase().replace('-', "_");
            let looks_secret = !key.is_empty()
                && SECRET_NAME_HINTS
                    .iter()
                    .any(|h| key_l == *h || key_l.ends_with(h) || key_l.starts_with(h));
            if !looks_secret {
                out.push_str(&rest[..sep_at + 1]);
                rest = &rest[sep_at + 1..];
                continue;
            }
            // 值：跳过分隔符后的空白
            let after = &rest[sep_at + 1..];
            let val_start = after.len() - after.trim_start_matches([' ', '\t']).len();
            let v = &after[val_start..];
            // 已经是占位符就别再涂了。不跳过的话重复处理（重试、导出）会让
            // 命中计数一路虚增——文本没变，但审计行会写「又发现 1 个密钥」。
            if v.starts_with(PLACEHOLDER_PREFIX) {
                let end = v
                    .find(PLACEHOLDER_SUFFIX)
                    .map(|e| e + PLACEHOLDER_SUFFIX.len());
                match end {
                    Some(e) => {
                        out.push_str(&rest[..sep_at + 1 + val_start + e]);
                        rest = &v[e..];
                        continue;
                    }
                    None => {
                        out.push_str(&rest[..sep_at + 1]);
                        rest = &rest[sep_at + 1..];
                        continue;
                    }
                }
            }
            let quoted = v.starts_with('"') || v.starts_with('\'');
            let val_len = if quoted {
                let q = v.as_bytes()[0] as char;
                v[1..].find(q).map(|e| e + 2).unwrap_or(v.len())
            } else if sep == ':' {
                // 「键: 值」是 HTTP 头那种形状，值可以带空格——`Authorization: Bearer abc.def`
                // 的秘密在第二个词上。按空白切会只涂掉 `Bearer` 而把 token 留在记录里，
                // 那正是第一版的表现。故这一支涂到行尾。
                v.len()
            } else {
                v.find([' ', '\t', ';', ',', ')', '&']).unwrap_or(v.len())
            };
            if val_len == 0 {
                out.push_str(&rest[..sep_at + 1]);
                rest = &rest[sep_at + 1..];
                continue;
            }
            out.push_str(&rest[..sep_at + 1]);
            out.push_str(&after[..val_start]);
            out.push_str(&placeholder("named-secret"));
            n += 1;
            rest = &v[val_len..];
        }
        result.push(out + nl);
    }
    (result.concat(), n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(text: &str) -> Redacted {
        redact(text, &[])
    }

    #[test]
    fn known_secrets_are_always_replaced() {
        // 这是本模块唯一的**强**保证：调用方明确交上来的密钥必被涂掉
        let out = redact("connect with hunter2 please", &["hunter2"]);
        assert!(!out.text.contains("hunter2"), "{}", out.text);
        assert!(out.text.contains("[[redacted:known-secret]]"));
        assert_eq!(out.total(), 1);
    }

    #[test]
    fn every_occurrence_of_a_known_secret_goes() {
        let out = redact("a hunter2 b hunter2 c", &["hunter2"]);
        assert!(!out.text.contains("hunter2"));
        assert_eq!(out.hits, vec![("known-secret", 2)]);
    }

    #[test]
    fn very_short_known_secrets_are_skipped_to_keep_the_record_readable() {
        // 口令是 `a` 时全文每个 a 都变占位符，审计记录直接不可读——
        // 那样的「脱敏」等于把证据毁掉
        let out = redact("cat /var/log/app.log", &["a", "og"]);
        assert_eq!(out.text, "cat /var/log/app.log");
        assert!(!out.any());
    }

    #[test]
    fn pem_private_key_blocks_are_removed_whole() {
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nAAAA\n-----END OPENSSH PRIVATE KEY-----";
        let out = r(&format!("here it is:\n{key}\nthat was it"));
        assert!(
            !out.text.contains("b3BlbnNzaC1rZXktdjEAAAAA"),
            "{}",
            out.text
        );
        assert!(out.text.starts_with("here it is:\n"));
        assert!(out.text.ends_with("\nthat was it"));
        assert_eq!(out.hits, vec![("private-key", 1)]);
    }

    #[test]
    fn a_truncated_key_block_is_still_removed() {
        // 终端上一屏刚好切在私钥中间：没有 END 标记。
        // 这种情况更要涂——剩下的全是密钥材料。
        let out = r("-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA0abcdef\n");
        assert!(
            !out.text.contains("MIIEpAIBAAKCAQEA0abcdef"),
            "{}",
            out.text
        );
        assert_eq!(out.total(), 1);
    }

    #[test]
    fn multiple_key_blocks_all_go() {
        let one = "-----BEGIN EC PRIVATE KEY-----\nAAA\n-----END EC PRIVATE KEY-----";
        let two = "-----BEGIN PRIVATE KEY-----\nBBB\n-----END PRIVATE KEY-----";
        let out = r(&format!("{one}\nmiddle\n{two}"));
        assert!(!out.text.contains("AAA"));
        assert!(!out.text.contains("BBB"));
        assert!(out.text.contains("middle"), "中间的正常文本不该被吞掉");
        assert_eq!(out.hits, vec![("private-key", 2)]);
    }

    #[test]
    fn url_credentials_lose_the_password_but_keep_the_user() {
        // 用户名对读审计的人有用（是谁的凭据），而它不是秘密
        let out = r("git clone https://deploy:s3cr3t@git.example.com/repo.git");
        assert!(!out.text.contains("s3cr3t"), "{}", out.text);
        assert!(out.text.contains("deploy:"), "用户名应保留：{}", out.text);
        assert!(out.text.contains("git.example.com/repo.git"));
        assert_eq!(out.hits, vec![("url-credential", 1)]);
    }

    #[test]
    fn urls_without_credentials_are_untouched() {
        for u in [
            "https://example.com/a/b?c=d",
            "http://localhost:8080/health",
            "ssh://git@github.com/o/r.git",
            "postgres://db.internal:5432/app",
        ] {
            let out = r(u);
            assert_eq!(out.text, u, "{u} 被改动了");
            assert!(!out.any(), "{u} 不该命中任何规则");
        }
    }

    #[test]
    fn prefixed_tokens_are_replaced() {
        let cases = [
            ("sk-abcdefghijklmnopqrstuvwx", "api-key"),
            ("ghp_abcdefghijklmnopqrstuvwxyz01", "github-token"),
            ("glpat-abcdefghijklmnopqr", "gitlab-token"),
            ("AKIAIOSFODNN7EXAMPLEXX", "aws-access-key"),
            ("xoxb-1234567890-abcdefghijkl", "slack-token"),
        ];
        for (tok, kind) in cases {
            let out = r(&format!("export K={tok}"));
            assert!(!out.text.contains(tok), "{tok} 没被涂掉：{}", out.text);
            assert!(
                out.text.contains(&placeholder(kind)) || out.text.contains("[[redacted:"),
                "{tok} 的占位符类别不对：{}",
                out.text
            );
        }
    }

    #[test]
    fn a_bare_prefix_mentioned_in_prose_is_not_a_token() {
        // 反向对照：文档里提到前缀本身不该触发。假阳性会把讲解文字涂掉，
        // 而一份读不懂的审计记录与没有审计记录的区别不大。
        for s in [
            "keys start with sk- followed by the body",
            "the AKIA prefix identifies access keys",
            "see ghp_ tokens in the docs",
        ] {
            let out = r(s);
            assert_eq!(out.text, s, "{s} 被误涂了");
        }
    }

    #[test]
    fn a_token_prefix_inside_a_longer_word_is_not_a_token() {
        // `ask-` 里含 `sk-`，但它不在词首
        let s = "please ask-someone-about-this-long-thing";
        assert_eq!(r(s).text, s);
    }

    #[test]
    fn named_assignments_lose_their_value() {
        for (input, gone) in [
            ("PASSWORD=hunter2", "hunter2"),
            ("export DB_PASSWORD=s3cret", "s3cret"),
            ("API_KEY=abc123def", "abc123def"),
            ("client_secret: swordfish", "swordfish"),
            ("Authorization: Bearer abc.def.ghi", "abc.def.ghi"),
            ("MYSQL_ROOT_PASSWORD='quoted pass'", "quoted pass"),
        ] {
            let out = r(input);
            assert!(
                !out.text.contains(gone),
                "{input} 里的 {gone} 没被涂掉：{}",
                out.text
            );
            assert!(out.any(), "{input} 应命中规则");
        }
    }

    #[test]
    fn the_key_name_survives_so_the_record_stays_useful() {
        // 涂值不涂名：读记录的人要知道「是哪个密钥」
        let out = r("DB_PASSWORD=hunter2");
        assert!(out.text.starts_with("DB_PASSWORD="), "{}", out.text);
    }

    #[test]
    fn ordinary_assignments_are_untouched() {
        // 这一组是假阳性的主要来源。全部必须原样通过。
        for s in [
            "PATH=/usr/bin:/bin",
            "HOME=/home/deploy",
            "count=42",
            "LANG=en_US.UTF-8",
            "git commit -m a1b2c3d4e5f6",
            "docker run -e NODE_ENV=production nginx",
            "curl http://localhost:8080/x",
            "C:\\Users\\deploy",
            "at 12:30 today",
            "ratio=1:2",
        ] {
            let out = r(s);
            assert_eq!(out.text, s, "{s} 被误改");
            assert!(!out.any(), "{s} 不该命中：{:?}", out.hits);
        }
    }

    #[test]
    fn clean_text_reports_zero_hits() {
        // 「涂了 0 处」与「涂了 3 处」对读记录的人意义完全不同：
        // 后者说明原文里有 3 个密钥，而那件事本身可能就是要调查的对象。
        let out = r("ls -la /var/log && systemctl status nginx");
        assert!(!out.any());
        assert!(out.hits.is_empty());
        assert_eq!(out.total(), 0);
    }

    #[test]
    fn known_secrets_run_before_shape_rules() {
        // 顺序有意义：若某条已知口令恰好长得像别的形状，先被形状规则涂掉，
        // 「已知密钥一定被涂掉」这个唯一的强保证就变成了偶然。
        let secret = "ghp_thisisaknownsecrettoken1";
        let out = redact(&format!("token {secret}"), &[secret]);
        assert!(!out.text.contains(secret));
        assert_eq!(
            out.hits,
            vec![("known-secret", 1)],
            "应记为 known-secret 而不是形状规则命中：{:?}",
            out.hits
        );
    }

    #[test]
    fn redaction_is_idempotent() {
        // 对已脱敏的文本再脱敏一次不该有变化——否则重复处理（重试、导出）
        // 会把占位符本身越涂越花
        let once = r("PASSWORD=hunter2 and sk-abcdefghijklmnopqrstuv");
        let twice = r(&once.text);
        assert_eq!(once.text, twice.text);
        assert!(!twice.any(), "第二遍不该再命中：{:?}", twice.hits);
    }

    #[test]
    fn multiline_input_is_handled_line_by_line() {
        let out = r("user=deploy\nPASSWORD=hunter2\nhost=db1\n");
        assert!(out.text.contains("user=deploy"));
        assert!(out.text.contains("host=db1"));
        assert!(!out.text.contains("hunter2"));
        // 行数不能变——审计记录要能与原始输出对齐行号
        assert_eq!(out.text.lines().count(), 3);
    }

    #[test]
    fn empty_and_huge_inputs_do_not_panic() {
        assert_eq!(r("").text, "");
        let big = "PASSWORD=x ".repeat(5000);
        let out = r(&big);
        assert!(out.total() >= 5000);
    }
}
