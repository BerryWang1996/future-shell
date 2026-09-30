//! 取证包（M4b 出口：「取证包：导出→重新导入校验 hash 链完整」）。
//!
//! # 取证包不是备份
//!
//! 这个区别是本模块的全部设计前提：
//!
//! - **备份**用来恢复。恢复审计库会**破坏 append-only 语义**——把库回滚到昨天，
//!   等于把今天发生的事抹掉，而那正是审计要防的事。所以本模块**没有** import-to-db。
//! - **取证包**用来给别人看。它离开这台机器，进到工单、邮件、法务流程里。
//!
//! 于是「重新导入」的含义只能是**校验**，不是写回。[`verify_bundle`] 不接触数据库。
//!
//! # 包必须自证
//!
//! 光把行导出来是不够的：收到包的人凭什么相信这就是当时库里的样子？
//! 所以包里除了行本身，还带三样东西——行数、链头 hash、链尾 hash。
//! 校验时重算整条链并与这三样比对：
//!
//! - 改包里任意一行的任意字段 → 该行的 hash 重算不上 → 定位到那一行；
//! - 删掉包里的一行 → 后一行的 prev_hash 接不上 → 定位到断口；
//! - 从尾部截掉几行 → 行数与链尾 hash 都不符；
//! - 整包重排 → 链序对不上。
//!
//! 单靠「每行自带 hash」挡不住后两种：截断后的包每一行都还自洽。
//!
//! # 包里有什么内容
//!
//! `action` 字段是命令文本。它在**写入审计时**就已经过脱敏（`fs_ai::redact`），
//! 但脱敏是形状匹配，挡不住一段看起来像散文的机密。取证包会离开本机，
//! 所以导出接口的文档必须把这件事说清楚，让调用方在 UI 上提醒用户——
//! 这不是能靠代码保证的事，故明确写在这里而不是假装已解决。

use crate::audit_repo::{AuditEntry, AuditRepo, BreakReason};
use crate::Error;

/// 取证包的格式版本。
///
/// 校验时**必须**比对：将来包格式变了（比如加字段进哈希），
/// 旧版程序拿新版包去校验会得到「链断了」的假警报，而那会让人以为审计被篡改过。
/// 带上版本就能给出准确的一句「这个包比本程序新」。
pub const BUNDLE_VERSION: u32 = 1;

/// 一份取证包。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ForensicBundle {
    pub version: u32,
    /// 导出时刻（RFC3339）。由调用方给——本模块不取当前时间，保持可测。
    pub exported_at: String,
    /// 产生这份包的程序版本，便于追溯。
    pub app_version: String,
    /// 行数。**参与自证**：从尾部截掉几行时，行数与链尾都会不符。
    pub row_count: usize,
    /// 链头（第一行的 hash）。空库时为 `None`。
    pub chain_head: Option<String>,
    /// 链尾（最后一行的 hash）。空库时为 `None`。
    pub chain_tail: Option<String>,
    /// 全部审计行，**按 id 升序**。顺序参与校验。
    pub rows: Vec<AuditEntry>,
}

/// 校验取证包的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleStatus {
    /// 包完好，且与它自己声明的头/尾/行数一致。
    Verified { rows: usize },
    /// 包的格式版本本程序处理不了。
    ///
    /// 单独一支而不是并进 `Tampered`：把「包比程序新」报成「审计被篡改」
    /// 是一个会造成误判的错误，而误判的代价是有人去查一件没发生过的事。
    UnsupportedVersion { found: u32, supported: u32 },
    /// 包被改过。
    Tampered { detail: TamperDetail },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TamperDetail {
    /// 某一行的内容与它自己的 hash 不符。
    RowAltered { at_id: i64 },
    /// 某一行的 prev_hash 与前一行的 hash 接不上。
    LinkBroken { at_id: i64 },
    /// 行数与声明不符。
    RowCountMismatch { declared: usize, actual: usize },
    /// 链头与声明不符。
    HeadMismatch,
    /// 链尾与声明不符（从尾部截断时命中这条）。
    TailMismatch,
    /// 行没有按 id 升序（整包被重排过）。
    OutOfOrder { at_id: i64 },
}

impl BundleStatus {
    /// 这个结论算不算「可以作为证据」。
    pub fn is_usable(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }

    /// 给人看的一句话。
    pub fn message(&self) -> String {
        match self {
            Self::Verified { rows } => {
                format!("取证包校验通过，共 {rows} 条审计记录，哈希链完整。")
            }
            Self::UnsupportedVersion { found, supported } => format!(
                "这份取证包的格式版本是 {found}，本程序只支持到 {supported}。\
                 请用更新的版本打开——这不代表包有问题。"
            ),
            Self::Tampered { detail } => match detail {
                TamperDetail::RowAltered { at_id } => {
                    format!("第 {at_id} 条记录的内容与它的哈希不符：这一行被改过。")
                }
                TamperDetail::LinkBroken { at_id } => {
                    format!("哈希链在第 {at_id} 条处断开：它前面有记录被删除或插入过。")
                }
                TamperDetail::RowCountMismatch { declared, actual } => {
                    format!("包声明有 {declared} 条记录，实际只有 {actual} 条：有记录被删掉了。")
                }
                TamperDetail::HeadMismatch => "链头与包声明的不符：开头的记录被换过。".into(),
                TamperDetail::TailMismatch => {
                    "链尾与包声明的不符：末尾的记录被截断或替换过。".into()
                }
                TamperDetail::OutOfOrder { at_id } => {
                    format!("第 {at_id} 条记录的顺序不对：包被重新排列过。")
                }
            },
        }
    }
}

/// 从审计库导出一份取证包。
///
/// # 包里含命令文本
///
/// `rows[].action` 是命令文本。它在写入审计时已经过脱敏，但脱敏是形状匹配，
/// 挡不住一段看起来像散文的机密。**这份包会离开本机**（进工单、邮件、法务流程），
/// 所以调用方应当在导出前于 UI 上明确提示这一点。
/// 代码保证不了这件事，故写在这里。
pub async fn export(
    repo: &AuditRepo<'_>,
    exported_at: &str,
    app_version: &str,
) -> Result<ForensicBundle, Error> {
    // 取全部行，按 id 升序——顺序参与校验，不能用 recent() 的倒序
    let rows = repo.all_ascending().await?;
    Ok(ForensicBundle {
        version: BUNDLE_VERSION,
        exported_at: exported_at.to_string(),
        app_version: app_version.to_string(),
        row_count: rows.len(),
        chain_head: rows.first().map(|r| r.hash.clone()),
        chain_tail: rows.last().map(|r| r.hash.clone()),
        rows,
    })
}

/// 校验一份取证包。**不接触数据库**——见模块头「取证包不是备份」。
pub fn verify_bundle(b: &ForensicBundle) -> BundleStatus {
    if b.version > BUNDLE_VERSION {
        return BundleStatus::UnsupportedVersion {
            found: b.version,
            supported: BUNDLE_VERSION,
        };
    }
    // ① 行数
    if b.row_count != b.rows.len() {
        return BundleStatus::Tampered {
            detail: TamperDetail::RowCountMismatch {
                declared: b.row_count,
                actual: b.rows.len(),
            },
        };
    }
    // ② 头尾
    if b.chain_head != b.rows.first().map(|r| r.hash.clone()) {
        return BundleStatus::Tampered {
            detail: TamperDetail::HeadMismatch,
        };
    }
    if b.chain_tail != b.rows.last().map(|r| r.hash.clone()) {
        return BundleStatus::Tampered {
            detail: TamperDetail::TailMismatch,
        };
    }
    // ③ 顺序：id 必须严格升序。重排后的包每行仍自洽，只有顺序能暴露它
    let mut last_id: Option<i64> = None;
    for r in &b.rows {
        if let Some(prev) = last_id {
            if r.id <= prev {
                return BundleStatus::Tampered {
                    detail: TamperDetail::OutOfOrder { at_id: r.id },
                };
            }
        }
        last_id = Some(r.id);
    }
    // ④ 逐行重算哈希链（复用与写入端**同一个**实现，见 audit_repo::verify_rows）
    match crate::audit_repo::verify_rows(&b.rows) {
        Ok(()) => BundleStatus::Verified { rows: b.rows.len() },
        Err((at_id, why)) => BundleStatus::Tampered {
            detail: match why {
                BreakReason::ContentAltered => TamperDetail::RowAltered { at_id },
                BreakReason::LinkMismatch => TamperDetail::LinkBroken { at_id },
            },
        },
    }
}

/// 从 JSON 文本读回一份包并校验（「重新导入」的完整含义）。
pub fn verify_json(json: &str) -> Result<BundleStatus, String> {
    let b: ForensicBundle =
        serde_json::from_str(json).map_err(|e| format!("取证包不是合法 JSON：{e}"))?;
    Ok(verify_bundle(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit_repo::{digest_output, Actor, Verdict};

    fn row(id: i64, prev: Option<&str>, action: &str) -> AuditEntry {
        // 用与写入端同一个哈希实现算出正确的 hash，于是这些行天生自洽
        let e = crate::audit_repo::NewAuditEntry {
            actor: Actor::Ai,
            action: action.into(),
            target_session: Some("s".into()),
            risk_level: "write".into(),
            verdict: Verdict::Approved,
            exit_code: Some(0),
            output_digest: Some(digest_output(b"o")),
            created_at: "2026-08-23T10:00:00Z".into(),
        };
        let hash = crate::audit_repo::row_hash_for_test(prev, &e);
        AuditEntry {
            id,
            actor: e.actor.as_str(),
            action: e.action,
            target_session: e.target_session,
            risk_level: e.risk_level,
            verdict: e.verdict.as_str().to_string(),
            exit_code: e.exit_code,
            output_digest: e.output_digest,
            created_at: e.created_at,
            prev_hash: prev.map(str::to_string),
            hash,
        }
    }

    fn bundle(n: usize) -> ForensicBundle {
        let mut rows = Vec::new();
        let mut prev: Option<String> = None;
        for i in 0..n {
            let r = row(i as i64 + 1, prev.as_deref(), &format!("cmd-{i}"));
            prev = Some(r.hash.clone());
            rows.push(r);
        }
        ForensicBundle {
            version: BUNDLE_VERSION,
            exported_at: "2026-08-23T12:00:00Z".into(),
            app_version: "0.4.1".into(),
            row_count: rows.len(),
            chain_head: rows.first().map(|r| r.hash.clone()),
            chain_tail: rows.last().map(|r| r.hash.clone()),
            rows,
        }
    }

    #[test]
    fn a_freshly_built_bundle_verifies() {
        let b = bundle(5);
        assert_eq!(verify_bundle(&b), BundleStatus::Verified { rows: 5 });
        assert!(verify_bundle(&b).is_usable());
    }

    #[test]
    fn an_empty_bundle_verifies() {
        // 空库导出的包是合法的。判成篡改会让一个刚装好的程序导出即报警。
        let b = bundle(0);
        assert_eq!(verify_bundle(&b), BundleStatus::Verified { rows: 0 });
    }

    #[test]
    fn json_round_trip_preserves_verification() {
        // 出口原文「导出→重新导入校验 hash 链完整」的直接对应
        let b = bundle(4);
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(
            verify_json(&json).unwrap(),
            BundleStatus::Verified { rows: 4 }
        );
    }

    #[test]
    fn editing_any_field_of_any_row_is_detected_and_located() {
        let mut b = bundle(5);
        b.rows[2].action = "被改过的命令".into();
        match verify_bundle(&b) {
            BundleStatus::Tampered {
                detail: TamperDetail::RowAltered { at_id },
            } => assert_eq!(at_id, 3, "指出的行号不对"),
            s => panic!("改行没被发现：{s:?}"),
        }
    }

    #[test]
    fn downgrading_a_dangerous_row_to_read_only_is_detected() {
        // 最有动机的那种篡改：把一条被拒绝的危险操作改成一条无害的只读操作
        let mut b = bundle(3);
        b.rows[1].risk_level = "read_only".into();
        b.rows[1].verdict = "auto_run".into();
        assert!(!verify_bundle(&b).is_usable());
    }

    #[test]
    fn deleting_a_middle_row_breaks_the_link() {
        let mut b = bundle(5);
        b.rows.remove(2);
        b.row_count = b.rows.len(); // 攻击者会把行数改对
                                    // 头尾也重新算对
        b.chain_head = b.rows.first().map(|r| r.hash.clone());
        b.chain_tail = b.rows.last().map(|r| r.hash.clone());
        match verify_bundle(&b) {
            BundleStatus::Tampered {
                detail: TamperDetail::LinkBroken { at_id },
            } => assert_eq!(
                at_id, 4,
                "删掉 id=3 之后，断链应当在紧随其后的那一行（id=4）被发现"
            ),
            s => panic!("删中间行没被发现：{s:?}"),
        }
    }

    /// **插一条**（交叉审计补：此前无用例）。
    ///
    /// 与「删一条」不是同一件事：删是让链上少一环，插是**多**一环，而插进去的
    /// 那条可以是攻击者精心编的（比如伪造一条「这次危险操作被批准了」）。
    /// 攻击者同样会把 row_count 与头尾改对——挡住它的只能是逐行重算。
    #[test]
    fn inserting_a_forged_row_breaks_the_link() {
        let mut b = bundle(5);
        // 伪造一条：id 挑在中间，内容是最有动机伪造的那种
        let mut forged = b.rows[2].clone();
        forged.id = 99;
        forged.action = "[伪造] 这条危险操作是被批准过的".into();
        forged.verdict = "approved".into();
        b.rows.insert(3, forged);
        b.row_count = b.rows.len();
        b.chain_head = b.rows.first().map(|r| r.hash.clone());
        b.chain_tail = b.rows.last().map(|r| r.hash.clone());
        match verify_bundle(&b) {
            BundleStatus::Tampered { detail } => assert!(
                matches!(
                    detail,
                    TamperDetail::RowAltered { .. }
                        | TamperDetail::LinkBroken { .. }
                        | TamperDetail::OutOfOrder { .. }
                ),
                "插入伪造行该被判成改行/断链/乱序，实得 {detail:?}"
            ),
            s => panic!("插入伪造行没被发现：{s:?}"),
        }
    }

    /// 插一条、且 **id 编得连续**（不触发乱序那条捷径）。
    ///
    /// 上一条实测被判成 `OutOfOrder`——因为 id=99 插在中间，序号先乱。那说明
    /// 乱序检查确实在工作，但它同时**掩盖**了链本身能不能挡住插入：一个懂行的
    /// 攻击者会把 id 也编顺。这一条把 id 编成连续的，逼校验器靠 hash 链本身发现。
    #[test]
    fn inserting_a_row_with_a_plausible_id_still_breaks_the_hash_chain() {
        let mut b = bundle(5);
        let mut forged = b.rows[2].clone();
        forged.action = "[伪造] 这条危险操作是被批准过的".into();
        forged.verdict = "approved".into();
        b.rows.insert(3, forged);
        // 把 id 全部重编成连续的——乱序检查从此看不出问题
        for (i, r) in b.rows.iter_mut().enumerate() {
            r.id = i as i64 + 1;
        }
        b.row_count = b.rows.len();
        b.chain_head = b.rows.first().map(|r| r.hash.clone());
        b.chain_tail = b.rows.last().map(|r| r.hash.clone());
        match verify_bundle(&b) {
            BundleStatus::Tampered { detail } => assert!(
                !matches!(detail, TamperDetail::OutOfOrder { .. }),
                "id 已编连续，不该再靠乱序发现——要靠 hash 链，实得 {detail:?}"
            ),
            s => panic!("id 编顺的插入没被发现——hash 链没挡住：{s:?}"),
        }
    }

    /// **改 prev_hash**（交叉审计补：此前无用例）。
    ///
    /// 这是「让删/插看起来合法」的必经一步——攻击者动完内容之后要把链重新接上。
    /// 只改 prev_hash 而不改自身 hash，重算时那一行的 hash 就对不上。
    #[test]
    fn rewriting_a_prev_hash_to_relink_the_chain_is_detected() {
        let mut b = bundle(5);
        b.rows[3].prev_hash = Some("0".repeat(64));
        match verify_bundle(&b) {
            BundleStatus::Tampered { detail } => assert!(
                matches!(
                    detail,
                    TamperDetail::RowAltered { at_id: 4 } | TamperDetail::LinkBroken { at_id: 4 }
                ),
                "改 prev_hash 应在 id=4 那行被发现，实得 {detail:?}"
            ),
            s => panic!("改 prev_hash 没被发现：{s:?}"),
        }
    }

    /// **改自身 hash**（交叉审计补：此前无用例）。
    ///
    /// 最直接的一种：内容不动，只把那一行记着的 hash 换掉，指望校验器信这个字段。
    /// 校验器必须**重算**而不是比对存下来的值——否则整条链就是一串自称。
    #[test]
    fn rewriting_a_rows_own_hash_is_detected_because_the_verifier_recomputes() {
        let mut b = bundle(5);
        b.rows[1].hash = "f".repeat(64);
        // 攻击者会顺手把下一行的 prev_hash 也改成同一个值，让链看起来仍然连着；
        // 挡住它的是「hash 必须由内容重算得出」这条，不是链的连续性。
        b.rows[2].prev_hash = Some("f".repeat(64));
        b.chain_tail = b.rows.last().map(|r| r.hash.clone());
        assert!(
            !verify_bundle(&b).is_usable(),
            "改自身 hash 必须被发现——校验器要重算，不能信这个字段"
        );
    }

    #[test]
    fn truncating_the_tail_is_detected_even_though_every_row_stays_self_consistent() {
        // 这是「每行自带 hash」挡不住的一种：截断后剩下的每一行都还自洽。
        // 只有行数与链尾能暴露它。
        let mut b = bundle(5);
        b.rows.truncate(3);
        // 攻击者不改声明字段：行数与尾 hash 都会对不上
        match verify_bundle(&b) {
            BundleStatus::Tampered {
                detail: TamperDetail::RowCountMismatch { declared, actual },
            } => {
                assert_eq!((declared, actual), (5, 3));
            }
            s => panic!("尾部截断没被发现：{s:?}"),
        }
    }

    #[test]
    fn truncating_the_tail_and_fixing_the_count_still_fails_on_the_tail_hash() {
        // 攻击者聪明一点：把行数也改对。链尾仍然对不上。
        let mut b = bundle(5);
        b.rows.truncate(3);
        b.row_count = 3;
        match verify_bundle(&b) {
            BundleStatus::Tampered {
                detail: TamperDetail::TailMismatch,
            } => {}
            s => panic!("改对行数后的截断没被发现：{s:?}"),
        }
    }

    #[test]
    fn replacing_the_head_is_detected() {
        let mut b = bundle(4);
        b.rows.remove(0);
        b.row_count = 3;
        b.chain_tail = b.rows.last().map(|r| r.hash.clone());
        // 头没改对
        match verify_bundle(&b) {
            BundleStatus::Tampered {
                detail: TamperDetail::HeadMismatch,
            } => {}
            s => panic!("换头没被发现：{s:?}"),
        }
    }

    #[test]
    fn reordering_the_bundle_is_detected() {
        // 重排后每一行仍自洽，只有 id 顺序能暴露它
        let mut b = bundle(5);
        b.rows.swap(1, 3);
        b.chain_head = b.rows.first().map(|r| r.hash.clone());
        b.chain_tail = b.rows.last().map(|r| r.hash.clone());
        match verify_bundle(&b) {
            BundleStatus::Tampered {
                detail: TamperDetail::OutOfOrder { .. },
            } => {}
            s => panic!("重排没被发现：{s:?}"),
        }
    }

    #[test]
    fn a_newer_bundle_version_is_reported_as_such_not_as_tampering() {
        // 把「包比程序新」报成「审计被篡改」会让人去查一件没发生过的事
        let mut b = bundle(2);
        b.version = BUNDLE_VERSION + 5;
        let s = verify_bundle(&b);
        assert_eq!(
            s,
            BundleStatus::UnsupportedVersion {
                found: BUNDLE_VERSION + 5,
                supported: BUNDLE_VERSION
            }
        );
        assert!(!s.is_usable());
        assert!(s.message().contains("不代表包有问题"), "{}", s.message());
    }

    #[test]
    fn an_older_bundle_version_is_still_accepted() {
        // 旧包要能一直打得开——取证材料的价值在于跨时间
        let mut b = bundle(2);
        b.version = 0;
        assert!(verify_bundle(&b).is_usable());
    }

    #[test]
    fn malformed_json_reports_a_parse_error_rather_than_tampering() {
        let e = verify_json("{not json").unwrap_err();
        assert!(e.contains("不是合法 JSON"), "{e}");
    }

    #[test]
    fn every_tamper_message_names_what_happened_and_where() {
        let all = [
            TamperDetail::RowAltered { at_id: 7 },
            TamperDetail::LinkBroken { at_id: 7 },
            TamperDetail::RowCountMismatch {
                declared: 5,
                actual: 3,
            },
            TamperDetail::HeadMismatch,
            TamperDetail::TailMismatch,
            TamperDetail::OutOfOrder { at_id: 7 },
        ];
        for d in all {
            let m = BundleStatus::Tampered { detail: d.clone() }.message();
            assert!(m.chars().count() >= 10, "{d:?} 的说明太短：{m}");
            // 每条都要说清「发生了什么」——只说「校验失败」的话没人知道下一步查什么
            let informative = ["改过", "断开", "删掉", "换过", "截断", "排列"]
                .iter()
                .any(|k| m.contains(k));
            assert!(informative, "{d:?} 的说明没说清发生了什么：{m}");
        }
    }
}
