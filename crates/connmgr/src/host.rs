//! 主机名规范化：信任库（`host_keys` / `revoked_host_keys`）以此作为存储与查询的键。
//!
//! ── 审计2 #21：主机密钥信任按**原始 host 字符串**精确匹配 ────────────────────────
//! 旧实现的键就是用户在连接配置里原样敲进去的那个串（`TrustStore::lookup` 的
//! `WHERE host = ?1`）。同一台主机在 SSH/DNS 语义上完全等价的几种写法，在库里是几个
//! 互不相干的键：
//!
//!   · `Example.COM` ↔ `example.com`      —— DNS 名大小写不敏感（RFC 4343）
//!   · `example.com.` ↔ `example.com`     —— 末尾的根点是合法 FQDN 写法
//!   · `[2001:db8::1]` ↔ `2001:db8::1`    —— 方括号是 `[host]:port` 语法的残留
//!   · `2001:DB8:0:0:0:0:0:1` ↔ `2001:db8::1` —— IPv6 的等价写法（RFC 5952 只有一种规范形）
//!
//! 后果是**静默 fail-open**：用户在一台主机上完成过 TOFU 确认，换一种拼法再连，
//! 信任库一行都查不到 → `decide` 判 `AskTofu` → 又弹一次「首次连接，是否信任」。
//! 真正的危害不是多点一次鼠标，而是它打破了「这个框只在第一次出现」这条用户唯一赖以
//! 判断的经验规则：一个每隔几次连接就冒出来一次的确认框，恰好把用户训练成无脑点接受——
//! 而中间人攻击那天，他看到的就是同一个框。
//!
//! 规范化只做**语义等价**的折叠，不做任何「看起来像」的猜测：
//!
//!   · 大小写只折 ASCII。绝不用 `to_lowercase()`：Unicode 大小写折叠会把两个视觉上
//!     不同、DNS 上也不同的标签映射到同一个键（土耳其语无点 i、`İ` 小写后变两个码位），
//!     那是在信任库里**制造**同形异义碰撞，方向与本次修复正好相反。
//!   · IP 字面量按解析结果重新渲染，非 IP 的串一个字节都不动（含 IDN/punycode：
//!     `xn--` 形式本就是 ASCII，原样即规范；未转换的 Unicode 域名保持原样，
//!     宁可多一条记录，也不替用户做 IDNA 转换这种会改变目标主机的判断）。

use std::net::{Ipv4Addr, Ipv6Addr};

/// 把一个用户输入的 host 串折成信任库的存储键。
///
/// 幂等：`canonical_host(canonical_host(h)) == canonical_host(h)`（由单测钉住）。
///
/// 刻意**不做**的事：不解析 DNS、不查 hosts 文件、不做 IDNA 转换。这三件事都会让
/// 「我信任的是哪台主机」取决于本机此刻的名字解析结果——而那正是攻击者最容易影响的一环。
pub fn canonical_host(host: &str) -> String {
    let mut h = host.trim();
    // `[2001:db8::1]` → `2001:db8::1`。只脱一层，且必须首尾成对，避免把 `[abc` 这类
    // 畸形输入悄悄改成另一个主机名。
    if let Some(inner) = h.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        h = inner;
    }
    // 末尾根点：`example.com.` 与 `example.com` 是同一个 FQDN。只脱一个——`example.com..`
    // 不是合法写法，把它折成同一个键等于替用户修正输入。
    if let Some(stripped) = h.strip_suffix('.') {
        h = stripped;
    }
    // IP 字面量：按 Rust 标准库的 Display 重新渲染。`Ipv6Addr` 的 Display 就是 RFC 5952
    // 规范形（小写、最长零段压缩成 `::`），于是同一个地址的所有写法收敛到唯一一个键。
    //
    // 两个解析器的文法互斥（v4 不含冒号，v6 必含冒号；`::ffff:1.2.3.4` 只有 v6 认），
    // 因此**先试哪个都不影响结果**——这里的次序不是正确性依赖，别把它当成一条被测性质。
    // 写成 v6 在前只是因为它是更长、更容易被误判的那一种，读代码时先看见它更省事。
    if let Ok(v6) = h.parse::<Ipv6Addr>() {
        return v6.to_string();
    }
    if let Ok(v4) = h.parse::<Ipv4Addr>() {
        return v4.to_string();
    }
    h.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::canonical_host;

    /// 大小写、根点、方括号、IPv6 压缩——四类等价写法各钉一条。
    /// 断言写成「两个不同的输入折到同一个输出」而不是「输出等于某个字面量」：
    /// 信任库真正需要的性质是**同一台主机只有一个键**，字面量长什么样是次要的。
    #[test]
    fn equivalent_spellings_fold_to_one_key() {
        assert_eq!(canonical_host("Example.COM"), canonical_host("example.com"));
        assert_eq!(
            canonical_host("example.com."),
            canonical_host("example.com")
        );
        assert_eq!(
            canonical_host("[2001:db8::1]"),
            canonical_host("2001:db8::1")
        );
        assert_eq!(
            canonical_host("2001:DB8:0:0:0:0:0:1"),
            canonical_host("2001:db8::1")
        );
        assert_eq!(
            canonical_host("  example.com  "),
            canonical_host("example.com")
        );
        // 具体形态也钉一条，免得「全都折成空串」这种实现同样满足上面每一条
        assert_eq!(canonical_host("Example.COM."), "example.com");
        assert_eq!(canonical_host("[2001:DB8::0:1]"), "2001:db8::1");
    }

    /// 不是同一台主机的，一个都不许折到一起。
    /// 缺了这一侧，`|_| String::new()` 能把上面那条测试全部通过。
    #[test]
    fn different_hosts_never_collide() {
        for (a, b) in [
            ("example.com", "example.org"),
            ("a.example.com", "b.example.com"),
            ("2001:db8::1", "2001:db8::2"),
            ("10.0.0.1", "10.0.0.2"),
            ("example.com", "www.example.com"),
        ] {
            assert_ne!(
                canonical_host(a),
                canonical_host(b),
                "{a} 与 {b} 被折到一起了"
            );
        }
    }

    /// 幂等。库里存的是规范形，下一次 open 再规范化一遍必须原地不动——
    /// 迁移改写路径（`Db::open`）每次启动都会跑一遍，不幂等就是每次启动改写一次数据。
    #[test]
    fn is_idempotent() {
        for h in [
            "Example.COM.",
            "[2001:DB8:0:0:0:0:0:1]",
            "127.0.0.1",
            "已备案.example",
            "",
            "xn--fsq.example",
        ] {
            let once = canonical_host(h);
            assert_eq!(canonical_host(&once), once, "{h} 的规范化不幂等");
        }
    }

    /// ASCII-only 折叠：Unicode 大小写折叠会制造同形异义碰撞，这里明确不做。
    /// `İ`（U+0130）的 `to_lowercase()` 是 `i` + U+0307 两个码位，会与普通 `i` 撞键。
    #[test]
    fn case_folding_is_ascii_only() {
        assert_eq!(canonical_host("İ.example"), "İ.example");
        assert_ne!(canonical_host("İ.example"), canonical_host("i.example"));
    }

    /// 畸形输入原样带过，不替用户「修正」成另一台主机。
    #[test]
    fn malformed_input_is_not_silently_repaired() {
        assert_eq!(canonical_host("[abc"), "[abc");
        assert_eq!(canonical_host("example.com.."), "example.com.");
        assert_eq!(canonical_host(""), "");
    }
}
