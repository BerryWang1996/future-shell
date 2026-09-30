use fs_connmgr::model::limits;
use fs_connmgr::{Db, HostKeyPin, HostKeyPolicy};
use fs_sshengine::hostkey::{
    decide, fingerprint_sha256, Decision, ImportSummary, PresentedKey, StoredKey, TrustStore,
};

fn presented(blob: &str) -> PresentedKey {
    PresentedKey {
        key_type: "ssh-ed25519".into(),
        key_blob: blob.into(),
        fingerprint_sha256: fingerprint_sha256(blob.as_bytes()),
    }
}

#[test]
fn tofu_first_contact_asks() {
    let d = decide(
        HostKeyPolicy::Tofu,
        &[],
        &[],
        &[],
        &presented("AAAAC3Nza-key1"),
    );
    assert!(matches!(d, Decision::AskTofu));
}

#[test]
fn known_match_accepts() {
    let k = presented("AAAAC3Nza-key1");
    let known = vec![StoredKey {
        key_type: k.key_type.clone(),
        key_blob: k.key_blob.clone(),
        fingerprint_sha256: k.fingerprint_sha256.clone(),
        source: "tofu".into(),
    }];
    assert!(matches!(
        decide(HostKeyPolicy::Tofu, &[], &known, &[], &k),
        Decision::Accept
    ));
}

#[test]
fn changed_key_is_flagged_with_old_fingerprint() {
    let old = presented("AAAAC3Nza-OLD");
    let known = vec![StoredKey {
        key_type: old.key_type.clone(),
        key_blob: old.key_blob.clone(),
        fingerprint_sha256: old.fingerprint_sha256.clone(),
        source: "tofu".into(),
    }];
    let d = decide(
        HostKeyPolicy::Tofu,
        &[],
        &known,
        &[],
        &presented("AAAAC3Nza-NEW"),
    );
    match d {
        Decision::Changed { old_fingerprint } => {
            assert_eq!(old_fingerprint, old.fingerprint_sha256)
        }
        other => panic!("expected Changed, got {other:?}"),
    }
}

#[test]
fn strict_refuses_unknown() {
    let d = decide(HostKeyPolicy::Strict, &[], &[], &[], &presented("x"));
    let Decision::Refuse { reason } = d else {
        panic!("expected Refuse, got {d:?}")
    };
    // 理由必须点名是「严格」策略在起作用：拒绝提示说错成因，用户就会去改错地方
    assert!(reason.contains("严格"), "拒绝理由须点名策略：{reason}");
}

#[test]
fn pin_takes_precedence() {
    let k = presented("AAAAC3Nza-key1");
    let pins = vec![HostKeyPin {
        key_blob: k.key_blob.clone(),
        fingerprint_sha256: k.fingerprint_sha256.clone(),
    }];
    assert!(matches!(
        decide(HostKeyPolicy::FingerprintPinned, &pins, &[], &[], &k),
        Decision::Accept
    ));
    // 审计 P1：钉不上是**硬拒绝**，不是可点接受的「密钥已变更」弹框。
    // 旧实现在这里返回 Changed，而钉扎分支从头到尾不读信任库——用户点「接受并记录」
    // 只会往信任库里写一行谁也不看的记录，下次连接同一个红框原样再来，永不收敛。
    assert!(matches!(
        decide(
            HostKeyPolicy::FingerprintPinned,
            &pins,
            &[],
            &[],
            &presented("other")
        ),
        Decision::Refuse { .. }
    ));
}

/// 审计 P1（`hostkey.rs:47`）：**策略 = 指纹钉扎 + 钉列表为空 ⇒ 恒硬拒绝**。
///
/// 这是本次修复的核心断言，也是旧实现最反直觉的一处：选了名义上最强的策略，
/// 拿到的保护反而**弱于**次强的 Strict——同样的输入，Strict 给硬拒绝，钉扎给一个
/// 可以点「接受并记录」放行的红框，且旧指纹是空字符串（一句假话：从来没有过旧密钥）。
///
/// 空钉在产品里是**可达且必然**的状态：ProfileDialog 的策略下拉有「指纹钉扎」，
/// 而 pins 只能删不能加（本次一并补了添加入口，见 `hostkey_known_keys` 命令）。
///
/// 第二个断言钉住「信任库整段不参与」：即便信任库里躺着这台主机**正确**的密钥，
/// 空白名单也不放行。空白名单不是「没有限制」，是「什么都不许过」——
/// 若在这里回落去查信任库，「钉扎」就退化成了 Tofu，而界面上写的仍是「已按钉扎策略校验」。
#[test]
fn pinned_policy_without_pins_refuses_instead_of_prompting() {
    let k = presented("AAAAC3Nza-key1");
    let known = [StoredKey {
        key_type: k.key_type.clone(),
        key_blob: k.key_blob.clone(),
        fingerprint_sha256: k.fingerprint_sha256.clone(),
        source: "tofu".into(),
    }];

    for (label, known_slice) in [("信任库为空", &[][..]), ("信任库有正确记录", &known[..])]
    {
        let d = decide(HostKeyPolicy::FingerprintPinned, &[], known_slice, &[], &k);
        let Decision::Refuse { reason } = d else {
            panic!("{label}：空钉必须硬拒绝，不得产出可点穿的弹框，got {d:?}")
        };
        // 理由必须让用户知道该去补一条钉，而不是去把策略调回 Tofu——
        // 后者正是一句笼统的「已拒绝」会导致的结果。
        assert!(
            reason.contains("未配置任何钉扎指纹"),
            "{label}：拒绝理由须点名「没配钉」这一成因：{reason}"
        );
    }
}

/// 非钉扎策略下，钉是**额外的已知良好密钥**：命中即免问，未命中则照常走信任库。
///
/// 旧实现是 `!pins.is_empty() || policy == FingerprintPinned` —— 只要配过钉就一律走钉扎分支，
/// 于是 Tofu + 有钉 + 出示一把没钉过的密钥，会对着一台**信任库里压根没见过**的主机
/// 谎称「密钥已变更」（old_fingerprint 取 `pins[0]`）。假告警和空告警是同一种伤害：
/// 都在训练用户无脑点接受。
#[test]
fn pins_under_non_pinned_policy_are_extra_known_good_keys() {
    let pinned_key = presented("AAAAC3Nza-PINNED");
    let pins = vec![HostKeyPin {
        key_blob: pinned_key.key_blob.clone(),
        fingerprint_sha256: pinned_key.fingerprint_sha256.clone(),
    }];

    // ① 命中钉 → 免问，即便信任库为空、策略为 Strict（用户亲手声明过这把是对的）
    assert!(matches!(
        decide(HostKeyPolicy::Strict, &pins, &[], &[], &pinned_key),
        Decision::Accept
    ));
    // ② 未命中钉 + 信任库为空 + Tofu → 如实报「首次见到」，不得谎称「已变更」
    assert!(matches!(
        decide(HostKeyPolicy::Tofu, &pins, &[], &[], &presented("OTHER")),
        Decision::AskTofu
    ));
    // ③ 未命中钉 + 信任库有异键 → Changed，且旧指纹取自**信任库**（真正见过的那把），
    //    而不是 pins[0]（那把从未被这台主机出示过）
    let old = presented("AAAAC3Nza-OLD");
    let known = vec![StoredKey {
        key_type: old.key_type.clone(),
        key_blob: old.key_blob.clone(),
        fingerprint_sha256: old.fingerprint_sha256.clone(),
        source: "tofu".into(),
    }];
    match decide(HostKeyPolicy::Tofu, &pins, &known, &[], &presented("OTHER")) {
        Decision::Changed { old_fingerprint } => assert_eq!(
            old_fingerprint, old.fingerprint_sha256,
            "旧指纹必须是信任库里那把真见过的密钥"
        ),
        other => panic!("expected Changed, got {other:?}"),
    }
}

#[test]
fn fingerprint_value_matches_ssh_keygen() {
    use base64::Engine;
    // 已知向量：ssh-keygen -t ed25519 -C tdd-vector 生成，公钥 base64 段原样粘贴；
    // 期望值经 ssh-keygen -lf 输出逐字核对（SHA256:WWGldbXAkapvaUjocuvVt0JLXky5hF80UshUCFxCoLE）。
    let blob_b64 = "AAAAC3NzaC1lZDI1NTE5AAAAIKVhi6rF2tuD2Oew3S77VEabPAN7FNgrrWKdlVymi5bC";
    let blob = base64::engine::general_purpose::STANDARD
        .decode(blob_b64)
        .unwrap();
    assert_eq!(
        fingerprint_sha256(&blob),
        "SHA256:WWGldbXAkapvaUjocuvVt0JLXky5hF80UshUCFxCoLE",
        "必须与 ssh-keygen -lf 输出逐字一致（对二进制 blob 求 SHA256，base64 无 padding）"
    );
}

#[tokio::test]
async fn truststore_persist_roundtrip() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let k = StoredKey {
        key_type: "ssh-ed25519".into(),
        key_blob: "blob1".into(),
        fingerprint_sha256: "SHA256:abc".into(),
        source: "tofu".into(),
    };
    ts.record("h", 22, &k, "tofu").await.unwrap();
    let got = ts.lookup("h", 22).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key_blob, "blob1");
}

#[tokio::test]
async fn accept_replaces_old_key() {
    // R13 替换语义：密钥变更显式接受走 replace——清退该 host:port 旧行，信任库只剩一行；
    // 旧键再出示时 decide 不命中 known → Changed（硬失败 + 弹框），而非静默 Accept（消除 fail-open MITM）
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let k1 = StoredKey {
        key_type: "ssh-ed25519".into(),
        key_blob: "blob-K1".into(),
        fingerprint_sha256: "SHA256:fp-K1".into(),
        source: "tofu".into(),
    };
    ts.record("h", 22, &k1, "tofu").await.unwrap();
    let k2 = StoredKey {
        key_type: "ssh-ed25519".into(),
        key_blob: "blob-K2".into(),
        fingerprint_sha256: "SHA256:fp-K2".into(),
        source: "tofu".into(),
    };
    ts.replace("h", 22, &k2).await.unwrap();
    let got = ts.lookup("h", 22).await.unwrap();
    assert_eq!(got.len(), 1, "替换后每 host:port 只应剩一行当前有效密钥");
    assert_eq!(got[0].key_blob, "blob-K2");
    assert!(
        got.iter().all(|k| k.key_blob != "blob-K1"),
        "旧键必须已退出信任库"
    );
    let d = decide(HostKeyPolicy::Tofu, &[], &got, &[], &presented("blob-K1"));
    assert!(
        matches!(d, Decision::Changed { .. }),
        "已退库旧键再出示必须判 Changed"
    );
}

#[tokio::test]
async fn record_replaces_same_host_port_keeps_single_row() {
    // R30 单行不变式：同 host:port 两次 record 异 key_blob（模拟并发首触 / MITM 抢答竞态），
    // 库中最终恰一行且为新键——record 与 replace 同构（单事务 DELETE 旧行 + INSERT），
    // PK 含 key_blob 不得令两行「当前有效」并存
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let k1 = StoredKey {
        key_type: "ssh-ed25519".into(),
        key_blob: "blob-A".into(),
        fingerprint_sha256: "SHA256:fp-A".into(),
        source: "tofu".into(),
    };
    ts.record("h", 22, &k1, "tofu").await.unwrap();
    let k2 = StoredKey {
        key_type: "ssh-ed25519".into(),
        key_blob: "blob-B".into(),
        fingerprint_sha256: "SHA256:fp-B".into(),
        source: "tofu".into(),
    };
    ts.record("h", 22, &k2, "tofu").await.unwrap();
    let got = ts.lookup("h", 22).await.unwrap();
    assert_eq!(
        got.len(),
        1,
        "同 host:port 两次 record 异 key 后必须恰剩一行（R30 单行不变式）"
    );
    assert_eq!(got[0].key_blob, "blob-B", "存行须为后记录的新键");
    assert!(
        got.iter().all(|k| k.key_blob != "blob-A"),
        "旧键必须已退出信任库"
    );
}

#[tokio::test]
async fn import_known_hosts_plain_lines() {
    use base64::Engine;
    // 合法 base64 fixture：SSH 线格式（u32 长度前缀 + 类型串 [+ 密钥数据]）二进制公钥 blob 的标准 base64 编码，
    // 与真实 OpenSSH known_hosts 行的 key 段结构一致；`general_purpose::STANDARD.decode` 必成功，红测可转绿。
    // 解码 = string("ssh-ed25519") + 32×0x01
    const KEY_ED25519_1: &str =
        "AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB";
    // 解码 = string("ssh-rsa") + e=65537 + 32×0x23
    const KEY_RSA_1: &str =
        "AAAAB3NzaC1yc2EAAAADAQABAAAAICMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMj";
    // 同主机异 key（冲突用例）
    const KEY_ED25519_2: &str =
        "AAAAC3NzaC1lZDI1NTE5AAAAIAICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgIC";
    // hashed 行整体跳过，blob 不被解码
    const KEY_HASHED: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIKqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
    // fixture 自检：先 base64::decode 成功，再按 known_hosts 行格式（hosts keytype blob [comment]）解析
    for blob in [KEY_ED25519_1, KEY_RSA_1, KEY_ED25519_2, KEY_HASHED] {
        base64::engine::general_purpose::STANDARD
            .decode(blob)
            .expect("fixture 须为合法标准 base64");
    }
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let content = format!(
        "\
# comment line
example.com ssh-ed25519 {KEY_ED25519_1} root@box
two.example.com,3.3.3.3 ssh-rsa {KEY_RSA_1}
|1|abcdef|ghijkl= ssh-ed25519 {KEY_HASHED} should-be-skipped
example.com ssh-ed25519 {KEY_ED25519_2} different-key-same-host
"
    );
    let s = ts.import_known_hosts(&content).await.unwrap();
    // 冲突行是 example.com 的第二把 **ed25519**：与既有行同族，择优保留没有偏好可言，
    // 因此仍旧原样计 conflicted、一个 upgraded 都不该有。
    // 断言整份结构（`..Default::default()` 把其余六格钉成 0），而不是挑几格看：
    // 这次修复的全部内容就是「每一行到底落到哪一格」，放过任何一格都等于放过一半。
    assert_eq!(
        s,
        ImportSummary {
            imported: 3,
            conflicted: 1,
            hashed: 1,
            ..Default::default()
        },
        "{s:?}"
    );
    let got = ts.lookup("example.com", 22).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key_blob, KEY_ED25519_1); // 冲突条目不得覆盖既有记录
    assert_eq!(got[0].source, "imported");
    let bin1 = base64::engine::general_purpose::STANDARD
        .decode(KEY_ED25519_1)
        .unwrap();
    assert_eq!(
        got[0].fingerprint_sha256,
        fingerprint_sha256(&bin1),
        "指纹须对 base64 解码后的二进制 blob 计算"
    );
    assert_eq!(
        ts.lookup("3.3.3.3", 22).await.unwrap().len(),
        1,
        "逗号 host 列表须逐一落库"
    );
    let s2 = ts.import_known_hosts(&content).await.unwrap();
    assert_eq!(
        s2,
        ImportSummary {
            duplicate: 3,
            conflicted: 1,
            hashed: 1,
            ..Default::default()
        },
        "重复导入幂等：三条已在库中的计 duplicate（审计2 #22：它与「被丢掉」不是一回事，\
         旧实现把两者并进同一个 skipped=4，用户无从分辨），冲突仍计 conflicted"
    );
}

/// 审计 P2：多算法主机导入后首连即弹假的「密钥已变更」。
///
/// 真实服务器普遍同时提供 ed25519 与 rsa 两把主机密钥，OpenSSH 的 known_hosts 里就有两行；
/// 而本机信任库钉死「每 host:port 至多一行」（R13/R30）。旧实现「先到的赢」，于是留下哪一把
/// 取决于**文件里哪一行排前面**——留下 rsa 时，首连协商出 ed25519，`decide` 只按 blob 比对便判
/// `Changed`，弹出与真 MITM 逐字相同的红框。用户为了连上只能点接受，而这一点，正是真出事那天
/// 唯一能救命的那一下。
///
/// 现在按真实协商顺序择优保留，且**与行序无关**：这里跑 rsa 在前的排法，
/// 下一条测试跑反过来的排法，两条必须落到同一个结果。
#[tokio::test]
async fn import_keeps_the_key_the_client_will_actually_negotiate() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let content = format!(
        "\
multi.example.com ssh-rsa {KEY_RSA}
multi.example.com ssh-ed25519 {KEY_ED25519}
"
    );
    let s = ts.import_known_hosts(&content).await.unwrap();
    assert_eq!(
        s,
        ImportSummary {
            imported: 1,
            upgraded: 1,
            ..Default::default()
        },
        "rsa 先落库、ed25519 顶替：一条 imported 一条 upgraded，不该有 conflicted（{s:?}）"
    );

    let got = ts.lookup("multi.example.com", 22).await.unwrap();
    assert_eq!(got.len(), 1, "不变式：每 host:port 至多一行");
    assert_eq!(
        (got[0].key_type.as_str(), got[0].key_blob.as_str()),
        ("ssh-ed25519", KEY_ED25519),
        "留下的必须是 russh 首选的 ed25519——留 rsa 就是首连假告警"
    );

    // 闭合到告警本身：库里这一行对上协商出的 ed25519，判 Accept 而非 Changed。
    let d = decide(
        HostKeyPolicy::Tofu,
        &[],
        &got,
        &[],
        &PresentedKey {
            key_type: "ssh-ed25519".into(),
            key_blob: KEY_ED25519.into(),
            fingerprint_sha256: "irrelevant".into(),
        },
    );
    assert!(
        matches!(d, Decision::Accept),
        "首连必须直接接受，不得弹「密钥已变更」：{d:?}"
    );
}

/// 同一份 known_hosts，两行顺序调过来，结果必须一模一样。
///
/// 这条不是上一条的复读：旧实现的全部毛病就是**结果由行序决定**，而行序是用户从来不会看、
/// 也无从控制的东西（OpenSSH 按首次连接顺序追加）。只测一种排法，等于把「先到的赢」
/// 换成「后到的赢」也照样绿。
#[tokio::test]
async fn which_key_survives_does_not_depend_on_the_file_order() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let content = format!(
        "\
multi.example.com ssh-ed25519 {KEY_ED25519}
multi.example.com ssh-rsa {KEY_RSA}
"
    );
    let s = ts.import_known_hosts(&content).await.unwrap();
    assert_eq!(
        s,
        ImportSummary {
            imported: 1,
            conflicted: 1,
            ..Default::default()
        },
        "ed25519 先落库，rsa 排不到它前面 → 计 conflicted，不顶替（{s:?}）"
    );

    let got = ts.lookup("multi.example.com", 22).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(
        (got[0].key_type.as_str(), got[0].key_blob.as_str()),
        ("ssh-ed25519", KEY_ED25519),
        "与上一条同样的结果：留下的那把与行序无关"
    );
}

/// 择优保留只在既有行也来自导入时才动手。
///
/// 用户亲手 TOFU 确认过的密钥是**人做出的判断**；一份 known_hosts 文件（可能是从别处拷来的、
/// 也可能是几年前的）不该有推翻它的权力。这里既有行是 tofu 的 rsa，导入的 ed25519 虽然排在
/// 更前，也必须止步于 conflicted。
///
/// 反过来说，这条也是「顶替」这一新行为的边界：少了 source 判定，导入一份文件就能悄悄改写
/// 用户确认过的信任关系，那是比原缺陷更糟的东西。
#[tokio::test]
async fn import_never_overwrites_a_key_the_user_confirmed_by_hand() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    ts.record(
        "hand.example.com",
        22,
        &StoredKey {
            key_type: "ssh-rsa".into(),
            key_blob: KEY_RSA.into(),
            fingerprint_sha256: "fp-rsa".into(),
            source: "tofu".into(),
        },
        "tofu",
    )
    .await
    .unwrap();

    let s = ts
        .import_known_hosts(&format!("hand.example.com ssh-ed25519 {KEY_ED25519}\n"))
        .await
        .unwrap();
    assert_eq!(
        s,
        ImportSummary {
            conflicted: 1,
            ..Default::default()
        },
        "更强的算法也不得顶替用户亲手确认过的行（{s:?}）"
    );

    let got = ts.lookup("hand.example.com", 22).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key_blob, KEY_RSA, "TOFU 行原封不动");
    assert_eq!(got[0].source, "tofu");
}

// ── 审计2 #21：TrustStore 的每个入口都折规范键 ─────────────────────────────────

/// 存进去用一种拼法，查出来用另一种，必须命中同一行。
///
/// 这是本次修复要消灭的那个静默 fail-open：旧实现 `WHERE host = ?1` 精确匹配原始串，
/// 于是 `Example.COM`、`example.com.`、`[2001:db8::1]` 各是一个互不相干的键——
/// 用户明明在这台主机上确认过，换个拼法连接却又弹一次 TOFU 框。危害不在多点一次鼠标，
/// 而在于「这个框只在第一次出现」这条经验规则被打破后，用户被训练成无脑点接受。
#[tokio::test]
async fn trust_lookup_is_immune_to_host_spelling() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let k = StoredKey {
        key_type: "ssh-ed25519".into(),
        key_blob: "blob-canon".into(),
        fingerprint_sha256: "SHA256:canon".into(),
        source: "tofu".into(),
    };
    ts.record("Example.COM.", 22, &k, "tofu").await.unwrap();
    for spelling in ["example.com", "EXAMPLE.com", "Example.COM.", "example.com."] {
        let got = ts.lookup(spelling, 22).await.unwrap();
        assert_eq!(got.len(), 1, "{spelling} 应命中同一行");
        assert_eq!(got[0].key_blob, "blob-canon");
    }
    // 不是同一台主机的，一个都不许命中——否则「全折成同一个键」也能让上面全绿。
    assert!(
        ts.lookup("example.org", 22).await.unwrap().is_empty(),
        "别的主机不得被折进来"
    );
    assert!(
        ts.lookup("example.com", 2222).await.unwrap().is_empty(),
        "端口仍须精确匹配"
    );

    // IPv6 的等价写法同样收敛；`replace` 也走同一条规范化路径。
    ts.record("[2001:DB8:0:0:0:0:0:1]", 22, &k, "tofu")
        .await
        .unwrap();
    let v6 = ts.lookup("2001:db8::1", 22).await.unwrap();
    assert_eq!(v6.len(), 1, "IPv6 的两种写法必须是同一个键");
    let k2 = StoredKey {
        key_blob: "blob-v6-new".into(),
        ..k.clone()
    };
    // 刻意用**第三种拼法**调 `replace`：写入口只要漏了规范化，DELETE 就打不中那一行，
    // 于是库里同时躺着新旧两把——「每 host:port 至多一行」当场破掉，而查询侧看到的还是旧的。
    ts.replace("[2001:DB8::1]", 22, &k2).await.unwrap();
    let after = ts.lookup("[2001:db8::1]", 22).await.unwrap();
    assert_eq!(after.len(), 1, "replace 后仍只有一行（R13/R30）");
    assert_eq!(after[0].key_blob, "blob-v6-new");
}

// ── 审计2 #22：`@revoked` 从「整行丢弃」变成硬拒绝 ──────────────────────────────

/// 吊销必须排在**钉扎与策略之前**。
///
/// 旧实现把 `@revoked` 行整行丢掉（只计一个笼统的 skipped），于是用户明确写下的
/// 「这把密钥永不得接受」，在本客户端退化成一个友好的「首次连接，是否信任？」确认框——
/// **安全控制反转**。现在它是 `decide` 的第一道判断，且给出不可点穿的 `Refuse`。
#[test]
fn revoked_key_is_refused_before_pins_and_policy() {
    let k = presented("AAAAC3Nza-LEAKED");
    let revoked = vec![StoredKey {
        key_type: k.key_type.clone(),
        key_blob: k.key_blob.clone(),
        fingerprint_sha256: k.fingerprint_sha256.clone(),
        source: "revoked".into(),
    }];
    // 一条**指向同一把密钥**的钉：陈旧的钉不得复活已知泄露的密钥。
    let pins = [HostKeyPin {
        key_blob: k.key_blob.clone(),
        fingerprint_sha256: k.fingerprint_sha256.clone(),
    }];
    // 一条同样指向它的信任记录：`known` 命中也不得放行。
    let known = vec![revoked[0].clone()];

    for (label, policy, p, kn) in [
        ("Tofu 无钉", HostKeyPolicy::Tofu, &[][..], &[][..]),
        (
            "Tofu + 信任库命中",
            HostKeyPolicy::Tofu,
            &[][..],
            &known[..],
        ),
        (
            "钉扎策略 + 钉命中",
            HostKeyPolicy::FingerprintPinned,
            &pins[..],
            &[][..],
        ),
        (
            "Strict + 钉命中",
            HostKeyPolicy::Strict,
            &pins[..],
            &known[..],
        ),
    ] {
        let d = decide(policy, p, kn, &revoked, &k);
        let Decision::Refuse { reason } = d else {
            panic!("{label}：吊销的密钥必须硬拒绝，got {d:?}")
        };
        assert!(
            reason.contains("吊销"),
            "{label}：拒绝理由须点名吊销这一成因：{reason}"
        );
    }

    // 反向：吊销名单里是**另一把**密钥时，判定不受任何影响。
    // 少了这一侧，`|…| Refuse` 这种「一律拒绝」的实现也能让上面全绿。
    let other = vec![StoredKey {
        key_blob: "AAAAC3Nza-SOMETHING-ELSE".into(),
        ..revoked[0].clone()
    }];
    assert!(
        matches!(
            decide(HostKeyPolicy::Tofu, &[], &[], &other, &k),
            Decision::AskTofu
        ),
        "别人的吊销记录不得殃及这把密钥"
    );
    assert!(
        matches!(
            decide(HostKeyPolicy::Tofu, &[], &known, &other, &k),
            Decision::Accept
        ),
        "吊销名单不命中时，信任库命中仍应 Accept"
    );
}

/// `@revoked` 行落库，且**同事务把信任库里那把摘掉**。
///
/// 摘除不是顺手清理：留着它，① `decide` 的检查次序哪天被调换，那把密钥立刻复活成 Accept；
/// ② `preferred_from_known` 会把**已吊销密钥**的算法族提到协商偏好最前，等于主动引导
/// 服务端出示我们最不想要的那一把。
#[tokio::test]
async fn importing_revoked_marker_blacklists_and_evicts_the_trusted_row() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());

    let s = ts
        .import_known_hosts(&format!("revoked.example ssh-ed25519 {KEY_ED25519}\n"))
        .await
        .unwrap();
    assert_eq!(
        s,
        ImportSummary {
            imported: 1,
            ..Default::default()
        },
        "先建立普通信任（{s:?}）"
    );
    assert_eq!(ts.lookup("revoked.example", 22).await.unwrap().len(), 1);

    // 吊销行**故意换一种拼法**写：`revoke` 自己不规范化的话，黑名单会落在
    // `Revoked.EXAMPLE.` 这个键上，而 `DELETE FROM host_keys` 也打不中已经建立的信任行——
    // 于是用户写下的「永不接受」既拦不住连接，又没把旧信任摘掉，两头落空。
    let s = ts
        .import_known_hosts(&format!(
            "@revoked Revoked.EXAMPLE. ssh-ed25519 {KEY_ED25519}\n"
        ))
        .await
        .unwrap();
    assert_eq!(
        s,
        ImportSummary {
            revoked: 1,
            ..Default::default()
        },
        "@revoked 行须计在 revoked 一格，不得混进 imported 或任何「跳过」（{s:?}）"
    );
    assert!(
        ts.lookup("revoked.example", 22).await.unwrap().is_empty(),
        "被吊销的密钥必须同时退出信任库"
    );
    let rev = ts.lookup_revoked("revoked.example", 22).await.unwrap();
    assert_eq!(rev.len(), 1);
    assert_eq!(rev[0].key_blob, KEY_ED25519);
    assert_eq!(rev[0].source, "revoked");

    // 幂等：同一份文件再导一次，计 duplicate 而非再加一条。
    let s = ts
        .import_known_hosts(&format!(
            "@revoked revoked.example ssh-ed25519 {KEY_ED25519}\n"
        ))
        .await
        .unwrap();
    assert_eq!(
        s,
        ImportSummary {
            duplicate: 1,
            ..Default::default()
        },
        "重复吊销须幂等（{s:?}）"
    );
    assert_eq!(
        ts.lookup_revoked("revoked.example", 22)
            .await
            .unwrap()
            .len(),
        1
    );

    // 已吊销的密钥，不因为同一份文件里还有一行普通条目就被请回信任库。
    let s = ts
        .import_known_hosts(&format!("revoked.example ssh-ed25519 {KEY_ED25519}\n"))
        .await
        .unwrap();
    assert_eq!(
        s,
        ImportSummary {
            duplicate: 1,
            ..Default::default()
        },
        "吊销优先于普通信任行（{s:?}）"
    );
    assert!(
        ts.lookup("revoked.example", 22).await.unwrap().is_empty(),
        "吊销的密钥不得被普通行请回信任库"
    );

    // 吊销的键同样过规范化：换个拼法查得到，才拦得住。
    assert_eq!(
        ts.lookup_revoked("Revoked.Example.", 22)
            .await
            .unwrap()
            .len(),
        1,
        "吊销名单的 host 键必须与信任库同一套规范化"
    );
}

/// 导入结果的每一格都有单一含义——旧实现把这六种结局塞进同一个 `skipped`。
///
/// 逐类各给一行，一次导入把九格全钉死。用户看到「跳过 6」时无从分辨自己的 known_hosts
/// 是本来就全在库里，还是有 6 行安全信息被整份丢掉了；而 `pattern` 那一行更糟：
/// 旧实现把 `*.example.com` 当**字面主机名**存进库里，「导入 N」里于是混着一条永远
/// 匹配不上任何真实主机的幽灵记录——它让用户以为整个域已经受保护，实际上没有。
#[tokio::test]
async fn every_unsupported_line_lands_on_its_own_counter() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let content = format!(
        "\
# 注释与空行不计数

plain.example ssh-ed25519 {KEY_ED25519}
@cert-authority ca.example ssh-ed25519 {KEY_ED25519}
@revoked bad.example ssh-ed25519 {KEY_RSA}
@no-such-marker x.example ssh-ed25519 {KEY_ED25519}
|1|abcdef|ghijkl= ssh-ed25519 {KEY_ED25519}
*.wild.example ssh-ed25519 {KEY_ED25519}
host?.q.example ssh-ed25519 {KEY_ED25519}
!negated.example ssh-ed25519 {KEY_ED25519}
missing-fields.example ssh-ed25519
notbase64.example ssh-ed25519 @@@not-base64@@@
"
    );
    let s = ts.import_known_hosts(&content).await.unwrap();
    assert_eq!(
        s,
        ImportSummary {
            imported: 1,
            cert_authority: 1,
            revoked: 1,
            hashed: 1,
            // `*` 通配、`?` 单字通配、`!` 取反——OpenSSH 的三种模式写法各一行。
            // `?` 单独占一行，否则去掉 `is_host_pattern` 里的 `?` 判据一条测试都不会红。
            pattern: 3,
            // `@no-such-marker`、字段不足、base64 解不开
            malformed: 3,
            ..Default::default()
        },
        "每一类都必须落在自己的计数上（{s:?}）"
    );

    // 计数说了不导入，库里就真的不能有。数字对得上、行却进去了，是更坏的一种谎。
    for host in [
        "ca.example",
        "x.example",
        "*.wild.example",
        "wild.example",
        "host?.q.example",
        "q.example",
        "!negated.example",
        "negated.example",
        "missing-fields.example",
        "notbase64.example",
        "bad.example",
    ] {
        assert!(
            ts.lookup(host, 22).await.unwrap().is_empty(),
            "{host} 不该出现在信任库里"
        );
    }
    assert_eq!(ts.lookup("plain.example", 22).await.unwrap().len(), 1);
}

/// `@revoked` 与 hashed 主机名同行时，标记要先剥掉才轮到 hashed 判断。
///
/// 旧代码只看行首那个 `|`，而这一行的行首是 `@`——于是它被当成普通主机名行，
/// `|1|…` 那串哈希被原样存成了「主机名」。这条单独钉，因为它正是两个判断的**次序**问题：
/// 把 hashed 检查放回标记之前，其余测试一条都不会红。
#[tokio::test]
async fn marker_is_stripped_before_the_hashed_check() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let s = ts
        .import_known_hosts(&format!(
            "@revoked |1|abcdef|ghijkl= ssh-ed25519 {KEY_ED25519}\n"
        ))
        .await
        .unwrap();
    assert_eq!(
        s,
        ImportSummary {
            hashed: 1,
            ..Default::default()
        },
        "标记须先剥离，剩下的哈希主机名照常计 hashed（{s:?}）"
    );
    assert!(
        ts.lookup("|1|abcdef|ghijkl=", 22).await.unwrap().is_empty(),
        "哈希串不得被当成主机名存进库里"
    );
}

/// 一行里逗号列了多台主机、blob 却解不开时，`malformed` 只该 +1（按行），不是按主机数。
/// 计数要与用户在编辑器里看到的行数对得上，否则「格式无法解析 5 行」会把人引向错误的一行。
#[tokio::test]
async fn a_bad_blob_counts_once_per_line_not_once_per_host() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let s = ts
        .import_known_hosts("a.example,b.example,c.example,d.example ssh-ed25519 @@bad@@\n")
        .await
        .unwrap();
    assert_eq!(
        s,
        ImportSummary {
            malformed: 1,
            ..Default::default()
        },
        "坏 blob 按行计一次（{s:?}）"
    );
}

/// 导入的 host 键同样过规范化：文件里写 `Example.COM.`，用 `example.com` 连接必须命中。
/// 少了这条，`import_known_hosts` 会成为唯一一个绕过规范化的写入口。
#[tokio::test]
async fn imported_hosts_are_stored_under_the_canonical_key() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let s = ts
        .import_known_hosts(&format!("Example.COM. ssh-ed25519 {KEY_ED25519}\n"))
        .await
        .unwrap();
    assert_eq!(
        s,
        ImportSummary {
            imported: 1,
            ..Default::default()
        },
        "{s:?}"
    );
    assert_eq!(ts.lookup("example.com", 22).await.unwrap().len(), 1);
    // 再导一次同一行的另一种拼法：应判 duplicate 而非又建一个键。
    let s = ts
        .import_known_hosts(&format!("EXAMPLE.com ssh-ed25519 {KEY_ED25519}\n"))
        .await
        .unwrap();
    assert_eq!(
        s,
        ImportSummary {
            duplicate: 1,
            ..Default::default()
        },
        "换个拼法的同一台主机同一把密钥，是 duplicate（{s:?}）"
    );
}

/// 解码 = string("ssh-ed25519") + 32×0x07。多算法用例共用的 ed25519 fixture。
const KEY_ED25519: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIAcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcH";
/// 解码 = string("ssh-rsa") + e=65537 + 32×0x09。与上面同一台主机的另一把密钥。
const KEY_RSA: &str = "AAAAB3NzaC1yc2EAAAADAQABAAAAIAkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJ";

/// S22（high）：唯一键不得只用「pid + 进程内计数器」——Windows 回收 pid、nextest 每测试一进程、
/// 且这些目录从不清理，跨轮残留 + pid 复用会让新测试直接跑在前人的终态库上（假红与假绿双向发生，
/// 详见 `crates/connmgr/tests/common/mod.rs` 的完整分析与回归测试）。
/// 故：名字加纳秒，且用 `create_dir`（已存在即报错）而非 `create_dir_all`，撞名换名重来。
fn tmpdir() -> std::path::PathBuf {
    static C: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = std::env::temp_dir();
    loop {
        let p = base.join(format!(
            "fs-hk-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时钟早于 UNIX 纪元")
                .as_nanos(),
            C.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        match std::fs::create_dir(&p) {
            Ok(()) => return p,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("创建测试临时目录失败 {}: {e}", p.display()),
        }
    }
}

// ── 审计2 #37：known_hosts 导入规模闸（与 JSON 导入同一把上限）───────────────────

/// 字节闸先于一切解析：内容根本不构成任何有效行，报的必须是 `Error::Import`（尺寸），
/// 而不是「0 行导入成功」之类把超大文件静默吞掉的结果。
#[tokio::test]
async fn import_known_hosts_rejects_oversized_content() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let content = "x".repeat(limits::IMPORT_MAX_BYTES + 1);
    match ts.import_known_hosts(&content).await {
        Err(fs_sshengine::Error::Import(m)) => {
            assert!(m.contains("字节上限"), "应报尺寸闸，实际：{m}")
        }
        other => panic!("应为 Error::Import（尺寸闸），实际：{other:?}"),
    }
}

/// 行数闸按**总行**计（空行注释行也算）：用户对着 `wc -l` 能核对自己错在哪。
/// 100_001 行注释远在 8 MiB 字节闸之内，能走到行数闸说明两层闸各司其职、互不遮蔽。
#[tokio::test]
async fn import_known_hosts_rejects_too_many_lines() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ts = TrustStore::new(db.pool());
    let content = "# padding\n".repeat(limits::IMPORT_MAX_LINES + 1);
    match ts.import_known_hosts(&content).await {
        Err(fs_sshengine::Error::Import(m)) => {
            assert!(m.contains("行上限"), "应报行数闸，实际：{m}")
        }
        other => panic!("应为 Error::Import（行数闸），实际：{other:?}"),
    }
}
