use crate::Error;
use base64::Engine;
use fs_connmgr::model::limits;
use fs_connmgr::{HostKeyPin, HostKeyPolicy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredKey {
    pub key_type: String,
    pub key_blob: String, // base64 公钥
    pub fingerprint_sha256: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct PresentedKey {
    pub key_type: String,
    pub key_blob: String,
    pub fingerprint_sha256: String,
}

#[derive(Debug, Clone)]
pub enum Decision {
    Accept,
    AskTofu,
    Changed {
        old_fingerprint: String,
    },
    /// **不可点穿**的硬拒绝。`reason` 说明拒绝依据，由 [`decide`] 就地产出并原样进 status 通道。
    ///
    /// 原变体名为 `RefuseStrict`，拒绝理由被硬编码成「strict 模式拒绝未知主机密钥」。
    /// 拒绝的成因不止 Strict 一种（钉扎策略下还有「未配置钉」与「不在钉扎列表中」两种），
    /// 而一条说错了成因的安全提示，其危害与没有提示相当：用户会去改 Strict 设置，
    /// 而真正该做的是补一条钉。理由必须在**知道理由的地方**生成。
    Refuse {
        reason: String,
    },
}

/// OpenSSH 指纹（与 `ssh-keygen -lf` 一致）：对**二进制**公钥 blob
/// （即 `public_key_base64()` 的 base64 解码结果，切勿对 base64 文本求）
/// 求 SHA256，输出 `SHA256:<base64 无 padding>`。
pub fn fingerprint_sha256(binary_key_blob: &[u8]) -> String {
    let digest = Sha256::digest(binary_key_blob);
    let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
    format!("SHA256:{b64}")
}

/// 主机密钥的**算法族**：把同一把公钥的多种签名算法写法折回同一个名字。
///
/// `ssh-rsa` / `rsa-sha2-256` / `rsa-sha2-512` 不是三把密钥，是**同一把 RSA 公钥**的三种签名
/// 算法，线上 blob 逐字节相同。known_hosts 里只会写 `ssh-rsa`，而 russh 的偏好里排在最前的
/// RSA 写法是 `rsa-sha2-512`。按名字比就会把同一把密钥当成两把，按族比才对得上。
fn host_key_family(name: &str) -> &str {
    match name {
        "ssh-rsa" | "rsa-sha2-256" | "rsa-sha2-512" => "ssh-rsa",
        other => other,
    }
}

/// 该算法族在客户端协商偏好里的位次（越小越先被服务端选中）；本客户端不提议的算法返回 `None`。
///
/// 顺序不另抄一份表，直接读 russh 的 `Preferred::DEFAULT.key`。抄一份就一定会在某次依赖升级后
/// 与真实协商顺序悄悄分家，而这个函数的全部价值恰恰在于「与实际协商结果一致」——分家之后它
/// 给出的每一个答案都是错的，且没有任何测试会因此变红。
pub fn host_key_family_rank(key_type: &str) -> Option<usize> {
    let fam = host_key_family(key_type);
    russh::Preferred::DEFAULT
        .key
        .iter()
        .position(|a| host_key_family(a.as_str()) == fam)
}

/// `candidate` 是否会比 `incumbent` 更早被协商选中。已知算法一律优于本客户端不提议的算法
///（后者协商不出来，留在库里等于永远对不上）。
fn outranks(candidate: &str, incumbent: &str) -> bool {
    match (
        host_key_family_rank(candidate),
        host_key_family_rank(incumbent),
    ) {
        (Some(a), Some(b)) => a < b,
        (Some(_), None) => true,
        _ => false,
    }
}

/// 把信任库里那把密钥的算法族整体提到协商偏好最前面——OpenSSH `order_hostkeyalgs()` 的同款做法。
///
/// ── 审计 P2：多算法主机的假「密钥已变更」告警 ────────────────────────────────
/// 信任库钉死「每 host:port 至多一行」（R13/R30：攻击者的密钥不得与真密钥并列长存），而真实
/// 服务器普遍同时提供 ed25519 与 rsa 两把主机密钥，用户的 known_hosts 里也就有两行。导入只能留
/// 一行，[`decide`] 又只按 blob 比对、不带算法维度，于是——库里留的是 rsa 那行，russh 默认偏好
/// 却先要 ed25519 → blob 不等 → `Changed` → 红色「主机密钥已变更，可能存在中间人攻击」。
///
/// 这是一次**纯假阳性**，而且与真 MITM 告警**逐字相同**。用户为了连上只能点接受，于是学会了
/// 对着这个红框点接受——真出事那天唯一能救命的那一下，已经被这次误报提前废掉了。
///
/// 修法不是放宽 `decide`（那才是真的削弱），而是**别让协商跑到另一把密钥上去**：连接前读一次
/// 信任库，把已记录的那一族排到偏好最前。服务器仍支持该算法 → 协商回同一把密钥 → 直接 Accept。
///
/// 只**重排**、不增删：没有任何算法因此被启用或禁用，降级面一寸未扩。RSA 一族内部的
/// `sha2-512 → sha2-256 → ssh-rsa` 次序也原样保留（`partition` 保序），签名哈希不会被拉低。
/// 攻击者也无从借力：偏好在握手**之前**由本地库决定，线上出示什么都改不了它。
///
/// 剩下的代价是真实的：库里若躺着 rsa，本函数会让这台主机**长期停在 rsa** 上。所以它必须与
/// [`TrustStore::import_known_hosts`] 的择优保留配套——导入时就把两行里更靠前的那把留下。
pub fn preferred_host_key_order(stored_key_type: &str) -> Vec<russh::keys::Algorithm> {
    let fam = host_key_family(stored_key_type);
    let (mut hoisted, rest): (Vec<_>, Vec<_>) = russh::Preferred::DEFAULT
        .key
        .iter()
        .cloned()
        .partition(|a| host_key_family(a.as_str()) == fam);
    hoisted.extend(rest);
    hoisted
}

/// 由信任库的查询结果直接给出该用哪套协商偏好；库里没有这台主机时用 russh 的默认序。
///
/// 存在的理由是**可测**：连接侧真正不可测的只剩「查库 + 把结果塞进 `client::Config`」这两行，
/// 而「查到 rsa 该怎么排」「查不到该怎么办」「重排会不会顺手改坏 kex/cipher/mac/compression」
/// 这些会出错的判断全落在这里，可以用普通单元测试钉住。
///
/// 只动 `key` 一项：其余四项由 `..Default::default()` 原样取自 russh，**不是**默认值语义的空表
///（那会把算法集清空、握手直接谈不拢）。这一点由测试守着。
pub fn preferred_from_known(known: &[StoredKey]) -> russh::Preferred {
    match known.first() {
        Some(k) => russh::Preferred {
            key: std::borrow::Cow::Owned(preferred_host_key_order(&k.key_type)),
            ..Default::default()
        },
        None => Default::default(),
    }
}

/// 主机密钥裁决。**钉扎策略下只有两种结局：`Accept` 或 `Refuse`——绝不产出可点穿的弹框。**
///
/// ── 审计 P1（`hostkey.rs:47`）：钉扎曾比「严格」更弱 ─────────────────────────────
/// 旧实现是 `if !pins.is_empty() || policy == FingerprintPinned { …不匹配就 Changed… }`，
/// 于是选了名义上最强策略的用户，拿到的保护反而弱于次强的 Strict：
///
///   · **空钉必然发生**：ProfileDialog 的策略下拉里有「指纹钉扎」，但该对话框只有
///     `removePin` 没有 addPin，「导入 known_hosts」写的是全局信任库而非 profile 钉，
///     `import_json` 还会显式 `host_key_pins.clear()`。选中该策略后 pins 恒为 `[]`。
///   · **空钉走到了 `Changed{old_fingerprint:""}`**：前端据此渲染红色「主机密钥已变更」，
///     旧指纹是一个空的 `<code>`。这是一句**假话**——从来没有过旧密钥。而 Strict 在同样
///     输入下给的是 `RefuseStrict` 硬拒绝，点不穿。
///   · **点穿之后不收敛**：用户点「接受并记录」→ `trust.replace` 把密钥写进**全局信任库**，
///     可钉扎分支从头到尾**不读信任库**。下次连接同一个红框原样再来一遍。于是这个告警
///     每次连接都出现、每次点接受都「没用」，恰好把用户训练成无脑点接受——
///     而这正是真出事那天唯一能救命的那一下。
///
/// 新语义按策略把 pins 的角色分开，两条都可判定、都收敛：
///
///   · `FingerprintPinned`：pins 就是**白名单**。命中 → `Accept`；未命中 → `Refuse`；
///     白名单为空 → 恒 `Refuse`（空白名单不是「没有限制」，是「什么都不许过」）。
///     信任库整段不参与——这正是「钉扎」区别于其它策略的地方，也是它该有的强度。
///   · 其余策略：pins 是**额外的已知良好密钥**。命中即免问，未命中则照常走信任库
///     （首见 → `AskTofu` / `RefuseStrict`，不一致 → `Changed`）。
///     这个方向只会让判定对「用户亲手声明过正确」的那把密钥更宽容，永远不会更危险；
///     且不再对着一台**信任库里压根没见过**的主机谎称「密钥已变更」。
///
/// ── 审计2 #22：`revoked` 排在最前，先于钉扎与策略 ─────────────────────────────
/// `revoked` 是 known_hosts 里 `@revoked` 标记过的密钥（见 [`TrustStore::revoke`]）。
/// OpenSSH 对它的表述是「must not ever be accepted」，没有任何例外，因此它必须是本函数
/// 的**第一道判断**：
///
///   · 排在钉扎之前——钉是「本机某一天确认过这把公钥」的历史断言，吊销是「这把公钥
///     此后不再可信」的新事实；让一条陈旧的钉复活一把已知泄露的密钥，是把两者的时间
///     关系搞反了。
///   · 排在策略之前——吊销与用户选了 TOFU 还是严格无关，它不是一档「强度」。
pub fn decide(
    policy: HostKeyPolicy,
    pins: &[HostKeyPin],
    known: &[StoredKey],
    revoked: &[StoredKey],
    presented: &PresentedKey,
) -> Decision {
    if let Some(k) = revoked.iter().find(|k| k.key_blob == presented.key_blob) {
        return Decision::Refuse {
            reason: format!(
                "这把主机密钥已被 known_hosts 的 `@revoked` 标记为吊销，任何情况下都不接受\
                 （出示 {}，算法 {}）。吊销通常意味着该密钥已泄露或已被管理员作废。\
                 若这台主机确实换了新密钥，请向管理员索取新的 known_hosts 行导入；\
                 不要为了连上而删掉这条吊销记录。",
                presented.fingerprint_sha256, k.key_type
            ),
        };
    }
    let pinned = pins.iter().any(|p| p.key_blob == presented.key_blob);
    if policy == HostKeyPolicy::FingerprintPinned {
        if pinned {
            return Decision::Accept;
        }
        // 两种拒绝理由必须分开说：「没配钉」要用户去补一条钉，「钉不上」是安全事件。
        // 合成一句笼统的「已拒绝」，用户只会去把策略调回 Tofu ——正好是我们最不想要的结果。
        return Decision::Refuse {
            reason: if pins.is_empty() {
                format!(
                    "主机密钥策略为「指纹钉扎」但该连接未配置任何钉扎指纹，无密钥可被接受：\
                     请在连接配置里添加一条钉扎指纹，或把策略改为「首次信任(TOFU)」/「严格」。\
                     （出示的密钥指纹 {}）",
                    presented.fingerprint_sha256
                )
            } else {
                format!(
                    "主机出示的密钥不在钉扎列表中，已按「指纹钉扎」策略硬拒绝（不可点击接受）：\
                     出示 {}，已钉扎 {} 条",
                    presented.fingerprint_sha256,
                    pins.len()
                )
            },
        };
    }
    // 非钉扎策略：钉是「已确认过的密钥」的额外来源，命中即免问。
    if pinned {
        return Decision::Accept;
    }
    match known.iter().find(|k| k.key_blob == presented.key_blob) {
        Some(_) => Decision::Accept,
        None if known.is_empty() => match policy {
            HostKeyPolicy::Strict => Decision::Refuse {
                reason: format!(
                    "「严格」策略拒绝未知主机密钥：本机信任库中没有这台主机的任何记录\
                     （出示的密钥指纹 {}）。请先导入 known_hosts，或改用「首次信任(TOFU)」策略。",
                    presented.fingerprint_sha256
                ),
            },
            _ => Decision::AskTofu,
        },
        None => Decision::Changed {
            old_fingerprint: known[0].fingerprint_sha256.clone(),
        },
    }
}

/// known_hosts 导入的逐类计数。
///
/// ── 审计2 #22：`skipped` 一个数字掩盖了五种性质完全不同的结局 ────────────────────
/// 旧结构只有 `imported / skipped / upgraded / conflicted`，而 `skipped` 同时承担着：
/// hashed 主机名、`@cert-authority`、`@revoked`、字段不足的畸形行、base64 解不开、
/// 以及「这条本来就已经在库里了」。前五种是**这条信任没有建立**，最后一种是**已经建立**——
/// 把它们加进同一个数字，用户看到「导入 3 / 跳过 12」时无法分辨自己的 known_hosts 是
/// 全都已经在库里了，还是有 12 行安全信息被整份丢掉了。
///
/// 现在按原因分列。前端据此逐项措辞：`duplicate` 说「已在库中」，`revoked` 说「已记为吊销」，
/// `cert_authority`/`hashed`/`pattern` 明说「本版本不支持，这些行没有进来」。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSummary {
    pub imported: u32,
    /// 同 (host, port) 已有一行**导入来的**弱算法密钥，被本行的更优算法顶替。
    ///
    /// 与 `conflicted` 分开计数，因为两者要对用户说的话完全相反：`conflicted` 是「有一行没进来，
    /// 你可能得知道」，`upgraded` 是「同一台主机的两行里我留了协商会用上的那把，你不必做什么」。
    /// 合并成一个数字，用户只会看到一个不知道该不该担心的「冲突 N」。
    pub upgraded: u32,
    pub conflicted: u32,
    /// 这条 (host, port, key_blob) 已经在库里 / 已经记为吊销 —— **信任已经建立**，无需任何动作。
    pub duplicate: u32,
    /// `@revoked` 行，已写入 `revoked_host_keys` 并从信任库摘除对应行。
    pub revoked: u32,
    /// `@cert-authority` 行：本版本不实现 SSH 证书验签，故**不导入**（详见 [`Marker`]）。
    pub cert_authority: u32,
    /// `|1|…` 哈希主机名行：哈希不可逆，无法还原成本库的查询键，故不导入。
    pub hashed: u32,
    /// 含 `*` / `?` 通配或 `!` 取反的 host 模式行：本库按精确键查询，不做模式匹配，故不导入。
    pub pattern: u32,
    /// 字段不足、`@` 后跟未知标记、base64 解不开——**格式**层面就不成立的行。
    pub malformed: u32,
}

/// known_hosts 行首的 `@` 标记（OpenSSH `sshd(8)` / `ssh_config(5)` 的 known_hosts 语法）。
///
/// ── 审计2 #22：`@revoked` 此前被整行丢弃，是**安全控制反转** ────────────────────
/// 旧代码见到 `hosts_field.starts_with('@')` 就 `skipped += 1` 掉头就走。对 `@cert-authority`
/// 而言这只是「少一种能力」；对 `@revoked` 而言，用户写下这一行的意思是「这把密钥永不得接受」，
/// 丢掉它之后，本客户端遇到同一把密钥给出的是一个友好的「首次连接，是否信任？」——
/// **用户明确的禁止，被翻译成了一次邀请。**
///
/// 两者的处置因此必须分开：`Revoked` 落进 `revoked_host_keys`（`decide` 第一道判断就拒），
/// `CertAuthority` 如实计数并说明不支持——绝不能当成普通主机密钥导入，那会把一把
/// **CA 公钥**注册成「这台主机的主机密钥」，从此真主机密钥反而被判 `Changed`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    None,
    CertAuthority,
    Revoked,
}

pub struct TrustStore<'a> {
    pool: &'a SqlitePool,
}

impl<'a> TrustStore<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// 查询这台主机已被信任的密钥。
    ///
    /// ── 审计2 #21：host 一律先过 [`canonical_host`] ──────────────────────────────
    /// 规范化放在 `TrustStore` 的**每一个入口**，而不是让调用方各自记得先折一次：
    /// 调用点分散在 connect.rs、conn_cmd.rs、导入路径和测试里，只要有一处忘了，
    /// 症状就是「明明信任过却又弹一次 TOFU」——一个不会让任何测试变红、只会让用户
    /// 逐渐学会无脑点接受的静默失效。收在这一层，忘不掉。
    pub async fn lookup(&self, host: &str, port: u16) -> Result<Vec<StoredKey>, Error> {
        let host = fs_connmgr::canonical_host(host);
        let rows = sqlx::query_as::<_, StoredKeyRow>(
            "SELECT key_type, key_blob, fingerprint_sha256, source FROM host_keys WHERE host = ?1 AND port = ?2",
        )
        .bind(&host)
        .bind(port as i64)
        .fetch_all(self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| StoredKey {
                key_type: r.key_type,
                key_blob: r.key_blob,
                fingerprint_sha256: r.fingerprint_sha256,
                source: r.source,
            })
            .collect())
    }

    /// 查询这台主机已被**吊销**的密钥（`@revoked`）。语义是黑名单，与 [`lookup`] 的白名单相反，
    /// 故不受「每 (host, port) 至多一行」的约束——一台主机换过几把泄露的密钥就有几条。
    ///
    /// [`lookup`]: TrustStore::lookup
    pub async fn lookup_revoked(&self, host: &str, port: u16) -> Result<Vec<StoredKey>, Error> {
        let host = fs_connmgr::canonical_host(host);
        let rows = sqlx::query_as::<_, RevokedKeyRow>(
            "SELECT key_type, key_blob, fingerprint_sha256 FROM revoked_host_keys WHERE host = ?1 AND port = ?2",
        )
        .bind(&host)
        .bind(port as i64)
        .fetch_all(self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| StoredKey {
                key_type: r.key_type,
                key_blob: r.key_blob,
                fingerprint_sha256: r.fingerprint_sha256,
                // 这一列在 `revoked_host_keys` 里不存在：吊销记录只有一个来源（导入的 `@revoked`），
                // 存一列恒等于 "revoked" 的字符串没有信息量。在此合成，让两张表返回同一个类型。
                source: "revoked".into(),
            })
            .collect())
    }

    /// 记录一把被吊销的主机密钥。返回 `true` 表示这是新记录，`false` 表示早已吊销过（幂等重导入）。
    ///
    /// **同事务里还要把它从信任库摘掉**，缺了这一步会留下两个真实的洞：
    ///   · `decide` 的检查次序哪天被调换（比如有人为了性能把 `known` 的命中提前），
    ///     一把躺在 `host_keys` 里的已吊销密钥立刻复活成 `Accept`；
    ///   · [`preferred_from_known`] 会把**这把已吊销密钥**的算法族提到协商偏好最前，
    ///     等于主动引导服务端出示那把密钥——正是我们最不想协商到的一把。
    pub async fn revoke(&self, host: &str, port: u16, key: &StoredKey) -> Result<bool, Error> {
        let host = fs_connmgr::canonical_host(host);
        let mut tx = self.pool.begin().await?;
        let ins = sqlx::query(
            r#"INSERT OR IGNORE INTO revoked_host_keys
                 (host, port, key_type, key_blob, fingerprint_sha256)
               VALUES (?1,?2,?3,?4,?5)"#,
        )
        .bind(&host)
        .bind(port as i64)
        .bind(&key.key_type)
        .bind(&key.key_blob)
        .bind(&key.fingerprint_sha256)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM host_keys WHERE host = ?1 AND port = ?2 AND key_blob = ?3")
            .bind(&host)
            .bind(port as i64)
            .bind(&key.key_blob)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(ins.rows_affected() > 0)
    }

    /// 首触接受（TOFU）落库：与 `replace` 同构的单事务写入（R30）——同一事务内先删除该 host:port
    /// 全部既存行再插入新行，与 `replace` 共同钉死「每 host:port 至多一行当前有效密钥」不变式：
    /// 并发首触（多标签同连同一新主机，或 MITM 抢答与真机并发）两次 record 异 key_blob 后，
    /// 库中恰一行且为新键；旧键再出示必触发 `decide()` Changed 硬失败，杜绝攻击者密钥与真密钥
    /// 并列被永久信任（spec §2.1/§6.1）。source 参数绑定不变，调用方零改动。
    pub async fn record(
        &self,
        host: &str,
        port: u16,
        key: &StoredKey,
        source: &str,
    ) -> Result<(), Error> {
        let host = fs_connmgr::canonical_host(host);
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM host_keys WHERE host = ?1 AND port = ?2")
            .bind(&host)
            .bind(port as i64)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            r#"INSERT INTO host_keys (host, port, key_type, key_blob, fingerprint_sha256, source, last_seen)
               VALUES (?1,?2,?3,?4,?5,?6, datetime('now'))"#,
        )
        .bind(&host)
        .bind(port as i64)
        .bind(&key.key_type)
        .bind(&key.key_blob)
        .bind(&key.fingerprint_sha256)
        .bind(source)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 密钥变更 + 用户显式接受 → 替换（supersede）语义（R13）：同一事务内删除该 host:port
    /// 全部旧行后插入新行（source 取 `key.source`）。信任库恒「每 host:port 至多一行当前有效密钥」；
    /// 旧键退库后再次出示时 `decide()` 不命中 known → `Changed` 硬失败 + 弹框，
    /// 消除 fail-open MITM（spec §2.1/§6.1）。与 `record` 并存：`record` 用于首触接受（TOFU），
    /// `replace` 用于 Changed 显式接受；AcceptOnce 仅内存态，两者均不影响。
    pub async fn replace(&self, host: &str, port: u16, key: &StoredKey) -> Result<(), Error> {
        let host = fs_connmgr::canonical_host(host);
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM host_keys WHERE host = ?1 AND port = ?2")
            .bind(&host)
            .bind(port as i64)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            r#"INSERT INTO host_keys (host, port, key_type, key_blob, fingerprint_sha256, source, last_seen)
               VALUES (?1,?2,?3,?4,?5,?6, datetime('now'))"#,
        )
        .bind(&host)
        .bind(port as i64)
        .bind(&key.key_type)
        .bind(&key.key_blob)
        .bind(&key.fingerprint_sha256)
        .bind(&key.source)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 导入 OpenSSH known_hosts 明文内容为初始信任集（spec §2.1；落库 source=imported）。
    /// 规则：注释/空行跳过（不计数）；逗号分隔 host 列表逐一落库；`[host]:port` 括号语法解析端口、
    /// 缺省 22。其余每一行都会落在 [`ImportSummary`] 的某一个具名计数上，没有「跳过」这个筐。
    ///
    /// 同 (host, port) 异 key 的处置（审计 P2「多算法主机的假变更告警」）：真实服务器普遍同时提供
    /// ed25519 与 rsa 两把主机密钥，用户的 known_hosts 里就有两行，而信任库只留一行（R13/R30）。
    /// 旧实现「先到的赢」，于是留下哪一把取决于**文件里哪一行排前面**——留下 rsa 时，首次连接
    /// 协商出 ed25519，[`decide`] 只按 blob 比对便判 `Changed`，弹出与真 MITM 逐字相同的红色告警。
    ///
    /// 现在按 [`host_key_family_rank`]（即真实协商顺序）择优：本行更靠前就顶替，计 `upgraded`；
    /// 否则维持既有行，计 `conflicted`。结果与文件里的行序无关——两种排法都留下 ed25519。
    ///
    /// 顶替只发生在既有行 `source == "imported"` 时。用户亲手 TOFU 确认过的密钥（`source = "tofu"`）
    /// 不因导入一份文件而被静默盖掉：那是人做出的判断，一份文件不该有推翻它的权力。
    ///
    /// ── 审计2 #22：本版本**不支持**的三类 known_hosts 语义，如实分列上报 ────────────
    /// hashed 主机名、`*`/`?`/`!` 主机模式、`@cert-authority` 都不导入，各计一个具名计数。
    /// 尤其是模式行：旧实现把 `*.example.com` 当作**字面主机名**存进库里，于是「导入 N」里
    /// 混着一条永远查不出来的幽灵记录——它让用户以为自己已经受保护了，而实际上没有。
    /// 少一个能力可以接受，谎报一个能力不行。
    pub async fn import_known_hosts(&self, content: &str) -> Result<ImportSummary, Error> {
        // 审计2 #37：前端 file.text() 把文件全量送进来，先量规模再逐行解析。
        // 上限与 JSON 导入同源（fs_connmgr::model::limits，单一定义，两处导入共用），
        // 挡的是「一次异常导入把进程内存打出峰值 / 行循环无限膨胀」。行数按**总行**计：
        // 用户对着 `wc -l` 就能核对自己错在哪，空行注释行也算（100k 行的 known_hosts
        // 无论如何都是异常文件，fail-closed 拒掉比猜「哪种行不算」更可预测）。
        if content.len() > limits::IMPORT_MAX_BYTES {
            return Err(Error::Import(format!(
                "导入文件超过 {} 字节上限",
                limits::IMPORT_MAX_BYTES
            )));
        }
        let mut s = ImportSummary::default();
        let mut line_no = 0usize;
        for raw in content.lines() {
            line_no += 1;
            if line_no > limits::IMPORT_MAX_LINES {
                return Err(Error::Import(format!(
                    "导入行数超过 {} 行上限",
                    limits::IMPORT_MAX_LINES
                )));
            }
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let mut first = parts.next().unwrap_or_default();
            // 标记先剥。剥完才轮到 hashed 判断——`@revoked |1|xx|yy ssh-rsa AAAA` 里的
            // `|1|` 在第二个字段上，旧代码只看行首那个 `|`，这一行会被当成普通主机名行处理。
            let marker = if let Some(name) = first.strip_prefix('@') {
                let m = match name {
                    "cert-authority" => Marker::CertAuthority,
                    "revoked" => Marker::Revoked,
                    // 未知的 `@…`：**不猜**。将来 OpenSSH 加了新标记，当成普通主机名导入
                    // 就是把一个我们读不懂的语义降级成「信任这台主机」。
                    _ => {
                        s.malformed += 1;
                        continue;
                    }
                };
                match parts.next() {
                    Some(f) => first = f,
                    None => {
                        s.malformed += 1;
                        continue;
                    }
                }
                m
            } else {
                Marker::None
            };
            let hosts_field = first;
            if hosts_field.starts_with('|') {
                s.hashed += 1;
                continue;
            }
            let (key_type, b64) = match (parts.next(), parts.next()) {
                (Some(t), Some(b)) => (t, b),
                _ => {
                    s.malformed += 1;
                    continue;
                }
            };
            // base64 解码提到**按行**做：blob 是整行共有的，一行里逗号列了 5 台主机时，
            // 旧代码会把同一个坏 blob 数成 5 次。计数要与用户看到的行数对得上。
            let fp = match base64::engine::general_purpose::STANDARD.decode(b64) {
                Ok(bin) => fingerprint_sha256(&bin),
                Err(_) => {
                    s.malformed += 1;
                    continue;
                }
            };
            if marker == Marker::CertAuthority {
                // 在拆 host 列表**之前**返回：CA 行按行计一次。它不是「某几台主机没进来」，
                // 是「这一行声明的是一把 CA 公钥，本版本不验 SSH 证书」。
                s.cert_authority += 1;
                continue;
            }
            for host_field in hosts_field.split(',') {
                if is_host_pattern(host_field) {
                    s.pattern += 1;
                    continue;
                }
                let (host, port) = parse_host_port(host_field);
                if host.is_empty() {
                    s.malformed += 1;
                    continue;
                }
                let key = StoredKey {
                    key_type: key_type.into(),
                    key_blob: b64.into(),
                    fingerprint_sha256: fp.clone(),
                    source: "imported".into(),
                };
                if marker == Marker::Revoked {
                    if self.revoke(&host, port, &key).await? {
                        s.revoked += 1;
                    } else {
                        s.duplicate += 1;
                    }
                    continue;
                }
                // 已吊销的密钥，不因为同一份文件里还有一行普通条目就被请回来。
                // 真实的 known_hosts 里这种并存完全可能（`@revoked` 行在下面、旧的信任行在上面），
                // 而两者相遇时该赢的一定是吊销。
                if self
                    .lookup_revoked(&host, port)
                    .await?
                    .iter()
                    .any(|k| k.key_blob == b64)
                {
                    s.duplicate += 1;
                    continue;
                }
                let known = self.lookup(&host, port).await?;
                if known.iter().any(|k| k.key_blob == b64) {
                    s.duplicate += 1;
                    continue;
                }
                // 同 (host, port) 已有一行、且 blob 不同。留哪一行不是随便挑的——
                // 「每 host:port 至多一行」是刻意的不变式，而 `decide` 只按 blob 比对，
                // 所以留下的那一行必须正是**协商真会用上**的那一族，否则首连即假告警。
                let superseded = match known.first() {
                    // 顶替既有行只在两个前提同时成立时才做：既有行也是**本机制导入的**
                    //（决不静默盖掉用户亲手 TOFU 确认过的密钥——那是人的判断，不是文件的），
                    // 且本行的算法族确实排在它前面。
                    Some(prev)
                        if prev.source == "imported" && outranks(key_type, &prev.key_type) =>
                    {
                        true
                    }
                    Some(_) => {
                        s.conflicted += 1;
                        continue;
                    }
                    None => false,
                };
                self.record(&host, port, &key, "imported").await?;
                // `record` 本身就是「同事务清空该 host:port 再插一行」，顶替不需要另一条写路径。
                if superseded {
                    s.upgraded += 1;
                } else {
                    s.imported += 1;
                }
            }
        }
        Ok(s)
    }
}

/// 这个 host 字段是 OpenSSH 的**模式**而非字面主机名吗？
///
/// 本库按精确键查询（`WHERE host = ?1`），无法表达模式匹配，因此模式行只计数、不导入。
/// 旧实现把它们当字面串存了进去：`*.example.com` 在库里躺成一条永远匹配不上任何真实
/// 主机名的记录，却让「导入 N」这个数字看起来一切正常——用户以为整个域已经受保护，
/// 实际上每一台机器首连时仍然是 TOFU。
///
/// `!` 取反同样只计数：把 `!secret.example.com` 当字面名导入，等于**信任了**一台用户
/// 明确写着「排除」的主机，方向正好相反。
fn is_host_pattern(field: &str) -> bool {
    field.starts_with('!') || field.contains('*') || field.contains('?')
}

/// known_hosts host 字段：`example.com` → (example.com, 22)；`[example.com]:2222` → (example.com, 2222)。
fn parse_host_port(field: &str) -> (String, u16) {
    if let Some(rest) = field.strip_prefix('[') {
        if let Some((h, p)) = rest.rsplit_once(']') {
            let port = p
                .strip_prefix(':')
                .and_then(|p| p.parse().ok())
                .unwrap_or(22);
            return (h.to_string(), port);
        }
    }
    (field.to_string(), 22)
}

#[derive(sqlx::FromRow)]
struct StoredKeyRow {
    key_type: String,
    key_blob: String,
    fingerprint_sha256: String,
    source: String,
}

#[derive(sqlx::FromRow)]
struct RevokedKeyRow {
    key_type: String,
    key_blob: String,
    fingerprint_sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RSA 的三种写法必须折成一族。这不是命名洁癖：known_hosts 里写 `ssh-rsa`，
    /// 而 russh 偏好里排最前的 RSA 写法是 `rsa-sha2-512`，两者是**同一把公钥**。
    /// 不折族的话，`preferred_host_key_order("ssh-rsa")` 只会把 `ssh-rsa` 这一个提上去，
    /// 服务器仍旧选 `rsa-sha2-512`，重排等于白做——而白做是看不出来的，连接照样成功。
    #[test]
    fn rsa_spellings_are_one_family() {
        let rank = host_key_family_rank("ssh-rsa");
        assert!(rank.is_some(), "ssh-rsa 必须在本客户端的提议列表里");
        assert_eq!(host_key_family_rank("rsa-sha2-512"), rank);
        assert_eq!(host_key_family_rank("rsa-sha2-256"), rank);
        assert_ne!(
            host_key_family_rank("ssh-ed25519"),
            rank,
            "ed25519 与 RSA 不是一族"
        );
    }

    /// 位次要与真实协商顺序同向：ed25519 先于 ecdsa 先于 rsa（russh `Preferred::DEFAULT.key`）。
    /// 这条同时钉住「顺序取自 russh 而非另抄一份表」——依赖升级改了顺序，这里会跟着变，
    /// 而抄来的表不会。
    #[test]
    fn rank_follows_the_real_negotiation_order() {
        let ed = host_key_family_rank("ssh-ed25519").expect("ed25519 在列");
        let ecdsa = host_key_family_rank("ecdsa-sha2-nistp256").expect("ecdsa-p256 在列");
        let rsa = host_key_family_rank("ssh-rsa").expect("rsa 在列");
        assert!(ed < ecdsa && ecdsa < rsa, "ed={ed} ecdsa={ecdsa} rsa={rsa}");
        assert_eq!(
            host_key_family_rank("ssh-dss"),
            None,
            "本客户端不提议 DSA，它排不出位次"
        );
    }

    #[test]
    fn outranks_prefers_earlier_and_treats_unproposed_as_last() {
        assert!(outranks("ssh-ed25519", "ssh-rsa"));
        assert!(!outranks("ssh-rsa", "ssh-ed25519"));
        assert!(!outranks("ssh-rsa", "rsa-sha2-512"), "同族不算更优");
        assert!(outranks("ssh-rsa", "ssh-dss"), "能协商的胜过协商不出来的");
        assert!(
            !outranks("ssh-dss", "ssh-dss"),
            "两边都协商不出来时不折腾既有行"
        );
    }

    /// 重排的两条硬约束：① 库里那一族整体到最前；② **一个算法都不能少**。
    /// 少一个就是悄悄禁用了一种算法——那是真正的降级，且只在遇到只支持该算法的服务器时才暴露。
    #[test]
    fn hoisting_reorders_without_adding_or_dropping_algorithms() {
        let base: Vec<String> = russh::Preferred::DEFAULT
            .key
            .iter()
            .map(|a| a.as_str().to_string())
            .collect();
        let hoisted = preferred_host_key_order("ssh-rsa");
        let names: Vec<String> = hoisted.iter().map(|a| a.as_str().to_string()).collect();

        let mut a = base.clone();
        let mut b = names.clone();
        a.sort();
        b.sort();
        assert_eq!(a, b, "只准重排，不准增删：{names:?} vs {base:?}");

        let rsa_count = base
            .iter()
            .filter(|n| host_key_family(n) == "ssh-rsa")
            .count();
        assert!(
            rsa_count >= 2,
            "RSA 一族本就有多种写法，否则这条测试没在测东西"
        );
        assert!(
            names[..rsa_count]
                .iter()
                .all(|n| host_key_family(n) == "ssh-rsa"),
            "RSA 一族必须整体排在最前：{names:?}"
        );
        assert_eq!(
            names[..rsa_count],
            base.iter()
                .filter(|n| host_key_family(n) == "ssh-rsa")
                .cloned()
                .collect::<Vec<_>>()[..],
            "族内次序（sha2-512 → sha2-256 → ssh-rsa）必须原样保留，签名哈希不得被拉低"
        );
    }

    /// 库里是 ed25519（russh 本来就首选）时，重排必须是恒等——否则这个函数在最常见的情形下
    /// 也在动协商偏好，出了事没人能一眼判断是不是它干的。
    #[test]
    fn hoisting_the_already_first_family_changes_nothing() {
        let base: Vec<String> = russh::Preferred::DEFAULT
            .key
            .iter()
            .map(|a| a.as_str().to_string())
            .collect();
        let names: Vec<String> = preferred_host_key_order("ssh-ed25519")
            .iter()
            .map(|a| a.as_str().to_string())
            .collect();
        assert_eq!(names, base);
    }

    /// 库里躺着一个本客户端根本不提议的算法（老库、或将来被移除的算法）时，
    /// 重排必须退化成恒等，而不是产出一个空的或残缺的提议列表——那会让握手直接失败。
    #[test]
    fn an_unproposed_stored_algorithm_leaves_the_order_untouched() {
        let base: Vec<String> = russh::Preferred::DEFAULT
            .key
            .iter()
            .map(|a| a.as_str().to_string())
            .collect();
        let names: Vec<String> = preferred_host_key_order("ssh-dss")
            .iter()
            .map(|a| a.as_str().to_string())
            .collect();
        assert_eq!(names, base, "无族可提时原样返回全表");
    }

    fn stored(key_type: &str) -> StoredKey {
        StoredKey {
            key_type: key_type.into(),
            key_blob: "AAAA".into(),
            fingerprint_sha256: "x".into(),
            source: "imported".into(),
        }
    }

    /// 库里没有这台主机（首连）时，必须原样交出 russh 的默认偏好。
    ///
    /// 这一条看着平凡，却是唯一挡住「`Default::default()` 写成了空 `Preferred`」的测试：
    /// 空表 = 一个算法都不提议 = 每一次首连都在密钥交换阶段直接失败。
    #[test]
    fn an_unknown_host_gets_the_stock_negotiation_order() {
        let p = preferred_from_known(&[]);
        let names: Vec<String> = p.key.iter().map(|a| a.as_str().to_string()).collect();
        let base: Vec<String> = russh::Preferred::DEFAULT
            .key
            .iter()
            .map(|a| a.as_str().to_string())
            .collect();
        assert_eq!(names, base);
        assert!(!names.is_empty(), "提议列表为空则握手必然失败");
    }

    /// 库里躺着 rsa 时，`key` 一项要被提族，**其余四项必须一字不动**。
    ///
    /// `russh::Preferred` 的 `Default` 就是 `DEFAULT`，所以 `..Default::default()` 是对的；
    /// 但这是一个「今天成立」的事实，不是签在接口上的承诺。哪天它变成 derive 出来的空表，
    /// 本函数会安静地把 kex/cipher/mac 全部清空，而只看 `key` 的测试一个都不会红。
    #[test]
    fn hoisting_touches_only_the_host_key_list() {
        let p = preferred_from_known(&[stored("ssh-rsa")]);
        let d = russh::Preferred::DEFAULT;

        assert_eq!(
            p.key.first().map(|a| a.as_str()),
            Some("rsa-sha2-512"),
            "rsa 族应被整体提到最前（族内仍以 sha2-512 打头）"
        );
        assert_eq!(format!("{:?}", p.kex), format!("{:?}", d.kex));
        assert_eq!(format!("{:?}", p.cipher), format!("{:?}", d.cipher));
        assert_eq!(format!("{:?}", p.mac), format!("{:?}", d.mac));
        assert_eq!(
            format!("{:?}", p.compression),
            format!("{:?}", d.compression)
        );
    }

    /// 同一台主机在库里只可能有一行（R13/R30 的不变式），但 `lookup` 返回的是切片；
    /// 万一将来放开成多行，这里的取舍必须是「第一行说了算」而不是悄悄取最后一行。
    #[test]
    fn the_first_row_decides_the_order() {
        let p = preferred_from_known(&[stored("ssh-rsa"), stored("ssh-ed25519")]);
        assert_eq!(p.key.first().map(|a| a.as_str()), Some("rsa-sha2-512"));
    }
}
