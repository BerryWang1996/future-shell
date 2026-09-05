//! SFTP 容器集成测试（Task 12 Step 6）：对真实 OpenSSH sftp-server 跑通 CRUD、软链三件套，
//! 以及传后校验四态 round-trip（路线图 M1 出口 / UI 规格 §1.4）。
//! 需 `FS_ITEST=1` + 可用 Docker；未设时打印 skip 并返回（与既有 itest 一致）。
use base64::Engine; // 解码 key blob 求指纹（`public_key_base64()` 返回文本，指纹须对二进制求）
use fs_itest::sshd::SshdContainer;
use fs_sshengine::sftp::{RemoteSftp, SftpOps};
use russh::client;
use russh::keys::PublicKeyBase64; // `public_key_base64()` 是 russh 扩展 trait 方法，不导入即 E0599
use std::sync::Arc;

// 复用 Task 9 的基线 handler：仅集成测试；产品路径禁止 accept-all（Task 10）
struct AcceptAllKeys;
impl russh::client::Handler for AcceptAllKeys {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        _server_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// 连接容器 → 密码认证 → 开 session channel → 请求 sftp 子系统 → 构造 RemoteSftp。
/// 返回容器 guard（必须绑定变量保活）与 RemoteSftp。
async fn open_sftp(tag: &str) -> (SshdContainer, RemoteSftp) {
    let sshd = SshdContainer::start(tag).await.unwrap();
    let mut session = client::connect(
        Arc::new(client::Config::default()),
        sshd.addr().await,
        AcceptAllKeys,
    )
    .await
    .unwrap();
    let res = session
        .authenticate_password(&sshd.username, &sshd.password)
        .await
        .unwrap();
    assert!(res.success(), "password auth failed: {res:?}");
    let channel = session.channel_open_session().await.unwrap();
    channel.request_subsystem(true, "sftp").await.unwrap();
    let sftp = RemoteSftp::new(channel).await.unwrap();
    (sshd, sftp)
}

#[tokio::test(flavor = "multi_thread")]
async fn sftp_crud_over_real_sshd() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, sftp) = open_sftp("sftp").await;
    // sftp-server 工作目录即用户家目录：全程用相对路径，避免硬编码 home
    sftp.write_at("t.bin", 0, b"data").await.unwrap(); // 1) 写往返
    assert_eq!(sftp.read_range("t.bin", 0, 1024).await.unwrap(), b"data"); // 2) 读往返
    assert_eq!(sftp.read_range("t.bin", 2, 1024).await.unwrap(), b"ta"); // 偏移读
    sftp.write_at("t.bin", 4, b"--more").await.unwrap(); // 偏移写不得截断（续传语义）
    assert_eq!(
        sftp.read_range("t.bin", 0, 1024).await.unwrap(),
        b"data--more"
    );
    assert_eq!(sftp.stat_size("t.bin").await.unwrap(), 10);

    sftp.mkdir("d").await.unwrap(); // 3) mkdir + list
    let listing = sftp.list(".").await.unwrap();
    // 审计2 #38：list 返回带截断标记的 ListResult——小目录必须完整，
    // 名字断言建立在「这是全貌」的前提上。
    assert!(!listing.truncated, "两文件一小目录不该截断");
    let entries = &listing.entries;
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"t.bin"), "list 缺 t.bin: {names:?}");
    assert!(names.contains(&"d"), "list 缺 d: {names:?}");
    assert!(entries.iter().find(|e| e.name == "d").unwrap().is_dir);
    assert!(
        !entries.iter().any(|e| e.name == "." || e.name == ".."),
        "`.`/`..` 应被过滤"
    );

    // symlink/lstat/read_link（OpenSSH sftp-server 默认放开 SSH_FXP_SYMLINK）
    // 注意入参序 (target, link_path)：见 sftp.rs `symlink` 的两次反转相消说明。
    // 本用例即该结论的**非空证明**——若参数写反，read_link("lnk.bin") 会拿不到 "t.bin"。
    sftp.symlink("t.bin", "lnk.bin").await.unwrap();
    let meta_l = sftp.lstat("lnk.bin").await.unwrap();
    assert_eq!(
        meta_l.file_type,
        fs_sshengine::sftp::FileType::Symlink,
        "lstat 不得跟随软链"
    );
    assert_eq!(sftp.read_link("lnk.bin").await.unwrap(), "t.bin");
    let meta_f = sftp.lstat("t.bin").await.unwrap(); // 常规文件 lstat
    assert_eq!(meta_f.file_type, fs_sshengine::sftp::FileType::Regular);
    assert_eq!(meta_f.size, 10);
    // mode 取自协议原始 permissions 低 12 位：普通文件必须落在 0o0000..=0o7777，
    // 且不得混入 0o100000 这类文件类型高位（若误用 permissions() 的 9 个 bool 重组，
    // setuid/setgid/sticky 会被静默抹掉——此处至少钉死取值域与非 None）。
    let mode = meta_f.mode.expect("OpenSSH 必回 permissions 属性");
    assert_eq!(mode & !0o7777, 0, "mode 须已剥离文件类型高位: {mode:o}");
    assert_ne!(mode & 0o600, 0, "属主至少可读写: {mode:o}");
    assert!(
        meta_f.uid.is_some() && meta_f.gid.is_some(),
        "OpenSSH 必回 uid/gid"
    );
    let listed = sftp.list(".").await.unwrap();
    assert!(!listed.truncated, "三文件一软链一小目录不该截断");
    assert!(
        listed
            .entries
            .iter()
            .find(|e| e.name == "lnk.bin")
            .unwrap()
            .is_symlink,
        "list 须标出软链"
    );

    sftp.rename("t.bin", "t2.bin").await.unwrap(); // rename
    assert_eq!(sftp.stat_size("t2.bin").await.unwrap(), 10);
    assert!(sftp.stat_size("t.bin").await.is_err());

    sftp.remove("t2.bin").await.unwrap(); // 4) remove 后 stat 报错
    assert!(sftp.stat_size("t2.bin").await.is_err());

    // 审计2 #17：目录删除必须能删（守卫递归），空目录也有 rmdir 路径。
    // 旧实现只调 SSH_FXP_REMOVE——目录在此必然失败（d 是 mkdir 建的空目录，非空后
    // remove_file 直接报错），「用户选中目录点删除」没有任何成功路径。
    sftp.write_at("d/f.bin", 0, b"x").await.unwrap();
    sftp.mkdir("d/sub").await.unwrap();
    sftp.write_at("d/sub/g.bin", 0, b"y").await.unwrap();
    fs_sshengine::sftp::remove_tree(&sftp, "d").await.unwrap();
    assert!(
        sftp.stat_size("d").await.is_err(),
        "递归删除后目录必须不存在"
    );
    sftp.mkdir("e").await.unwrap(); // 空目录的删除路径（RMDIR）
    fs_sshengine::sftp::remove_tree(&sftp, "e").await.unwrap();
    assert!(sftp.stat_size("e").await.is_err(), "空目录也必须可删");
}

/// 传后校验 round-trip（路线图 M1 出口 / UI 规格 §1.4）：对真实 OpenSSH 容器逐字覆盖
/// 「sha256 匹配 / 篡改失配 / 降级 size 匹配 / 降级 size 失配」四态（禁静默跳过）。
struct ContainerExec {
    session: tokio::sync::Mutex<russh::client::Handle<AcceptAllKeys>>,
}

#[async_trait::async_trait]
impl fs_sshengine::verify::ExecChannel for ContainerExec {
    async fn exec_once(
        &self,
        cmd: &str,
    ) -> Result<fs_sshengine::verify::ExecOutput, fs_sshengine::Error> {
        let mut ch = self
            .session
            .lock()
            .await
            .channel_open_session()
            .await
            .map_err(|e| fs_sshengine::Error::Ssh(e.to_string()))?;
        ch.exec(false, cmd)
            .await
            .map_err(|e| fs_sshengine::Error::Ssh(e.to_string()))?;
        Ok(fs_sshengine::verify::run_exec_channel(&mut ch).await)
    }
}

/// 强制降级适配器：拦截 sha256sum 模拟服务端无该命令（exit 127）——驱动 size 降级路径。
struct NoSha256sum<'a>(&'a ContainerExec);

#[async_trait::async_trait]
impl fs_sshengine::verify::ExecChannel for NoSha256sum<'_> {
    async fn exec_once(
        &self,
        cmd: &str,
    ) -> Result<fs_sshengine::verify::ExecOutput, fs_sshengine::Error> {
        if cmd.starts_with("sha256sum") {
            return Ok(fs_sshengine::verify::ExecOutput {
                code: Some(127),
                stdout: String::new(),
                stderr: "sh: sha256sum: not found".to_string(),
            });
        }
        self.0.exec_once(cmd).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn transfer_verify_roundtrip_against_real_sshd() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    use fs_sshengine::transfer::Direction;
    use fs_sshengine::verify::{run_verify, ExecChannel, VerifyEntry, VerifyOutcome, VerifyPlan};

    let sshd = SshdContainer::start("verify").await.unwrap();
    let mut session = client::connect(
        Arc::new(client::Config::default()),
        sshd.addr().await,
        AcceptAllKeys,
    )
    .await
    .unwrap();
    let res = session
        .authenticate_password(&sshd.username, &sshd.password)
        .await
        .unwrap();
    assert!(res.success(), "password auth failed: {res:?}");
    let sftp_ch = session.channel_open_session().await.unwrap();
    sftp_ch.request_subsystem(true, "sftp").await.unwrap();
    let sftp = RemoteSftp::new(sftp_ch).await.unwrap();
    let exec = ContainerExec {
        session: tokio::sync::Mutex::new(session),
    };

    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("v.bin");
    let payload = b"future-shell verify payload"; // 27 字节
    std::fs::write(&local, payload).unwrap();
    // sftp-server 工作目录即家目录：相对路径（与 sftp_crud 一致）
    sftp.write_at("v.bin", 0, payload).await.unwrap();
    let expect = fs_sshengine::verify::file_sha256(&local).await.unwrap();

    let entry = VerifyEntry {
        direction: Direction::Up,
        remote: "v.bin".into(),
        local: local.clone(),
        plan: VerifyPlan {
            expect: Some(expect.clone()),
            degraded: false,
        },
    };
    // ⓪ 退出码必达（S41 直接回归）：`run_exec_channel` 若在 `ChannelMsg::Eof` 处 break，
    // 真实 OpenSSH 随后才发的 exit-status 会被整条丢掉、code 恒为 -1，其后所有 sha256
    // 路径都静默降级成 size 比对。此断言把「退出码真的收到了」钉在 `run_verify` 之前，
    // 使该回归以「-1 != 0」而不是以「结局是 SizeOnlyMatch」这种远端症状暴露。
    let eo = exec.exec_once("sha256sum -- 'v.bin'").await.unwrap();
    let (code, out, err) = (eo.code, eo.stdout, eo.stderr);
    assert_eq!(
        code,
        Some(0),
        "exec 退出码须真实送达（code={code:?} stdout={out:?} stderr={err:?}）"
    );
    assert_eq!(
        fs_sshengine::verify::parse_sha256sum(&out),
        Some(expect.clone()),
        "容器 sha256sum 输出须与本地哈希逐字相等"
    );
    // ① 真实服务器 sha256 路径：匹配（linuxserver 镜像 busybox 自带 sha256sum）
    // 断言一律用 assert_eq! 而非 assert!(matches!(..))：后者失败时只打印源码文本，
    // 拿不到实际结局，把「降级成 SizeOnlyMatch」和「Unverified」混成同一条无信息的红。
    assert_eq!(
        run_verify(&exec, &sftp, &entry).await,
        VerifyOutcome::Sha256Match
    );
    // ② 篡改远端（追加 1 字节）→ 失配
    sftp.write_at("v.bin", 27, b"X").await.unwrap();
    assert_eq!(
        run_verify(&exec, &sftp, &entry).await,
        VerifyOutcome::Mismatch
    );
    // ③ 恢复内容 + 拦截 sha256sum 模拟命令缺失 → 降级 size，大小一致
    sftp.remove("v.bin").await.unwrap();
    sftp.write_at("v.bin", 0, payload).await.unwrap();
    assert_eq!(
        run_verify(&NoSha256sum(&exec), &sftp, &entry).await,
        VerifyOutcome::SizeOnlyMatch
    );
    // ④ 降级路径大小失配 → Mismatch（不静默）
    sftp.write_at("v.bin", 27, b"XX").await.unwrap();
    assert_eq!(
        run_verify(&NoSha256sum(&exec), &sftp, &entry).await,
        VerifyOutcome::Mismatch
    );
}

/// R30 单行不变式容器回归：并发首触（多标签同连同一新主机，或 MITM 抢答与真机竞态）
/// 对同 host:port 两次 record 异 key_blob，信任库必须恰一行且为新键——record 与 replace 同构
/// （单事务 DELETE 同 host:port 旧行 + INSERT）。复用容器 fixture：握手抓取真实宿主密钥作首触键，
/// 再以攻击者键模拟抢答 record。
#[tokio::test(flavor = "multi_thread")]
async fn hostkey_record_keeps_single_row_per_host_port() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("tofu-row").await.unwrap();
    let addr = sshd.addr().await;
    let host = addr.ip().to_string();
    let dir = tempfile::tempdir().unwrap();
    let db = fs_connmgr::Db::open(&dir.path().join("fs.db"))
        .await
        .unwrap();
    let trust = fs_sshengine::hostkey::TrustStore::new(db.pool());

    // ① 握手抓取容器真实宿主密钥，作首触记录（等价 TOFU AcceptAndRecord 的落库路径）
    #[derive(Clone, Default)]
    struct GrabKey {
        key: Arc<std::sync::Mutex<Option<russh::keys::PublicKey>>>,
    }
    impl russh::client::Handler for GrabKey {
        type Error = russh::Error;
        async fn check_server_key(
            &mut self,
            server_key: &russh::keys::PublicKey,
        ) -> Result<bool, Self::Error> {
            *self.key.lock().unwrap() = Some(server_key.clone());
            Ok(true)
        }
    }
    let grab = GrabKey::default();
    let _session = client::connect(Arc::new(client::Config::default()), addr, grab.clone())
        .await
        .unwrap();
    let real = grab
        .key
        .lock()
        .unwrap()
        .clone()
        .expect("握手必须取得容器宿主密钥");
    // 指纹对二进制 key blob 求 SHA256——照搬 connect.rs `check_server_key` 正规范式：先 base64 解码，
    // 切勿对文本求；`PublicKey::fingerprint()` 需 HashAlg 实参且返回 `Fingerprint` 而非 String，不可零参调用
    let real_blob = base64::engine::general_purpose::STANDARD
        .decode(real.public_key_base64())
        .expect("宿主密钥 base64 必须可解码");
    trust
        .record(
            &host,
            addr.port(),
            &fs_sshengine::hostkey::StoredKey {
                key_type: real.algorithm().as_str().to_string(),
                key_blob: real.public_key_base64(),
                fingerprint_sha256: fs_sshengine::hostkey::fingerprint_sha256(&real_blob),
                source: "tofu".into(),
            },
            "tofu",
        )
        .await
        .unwrap();

    // ② 模拟并发首触 / MITM 抢答：同 host:port 以异 key_blob 再次 record（攻击者密钥或轮换后密钥）
    let k2 = b"K2-attacker-or-rotated-key";
    trust
        .record(
            &host,
            addr.port(),
            &fs_sshengine::hostkey::StoredKey {
                key_type: "ssh-ed25519".into(),
                key_blob: String::from_utf8_lossy(k2).into_owned(),
                fingerprint_sha256: fs_sshengine::hostkey::fingerprint_sha256(k2),
                source: "tofu".into(),
            },
            "tofu",
        )
        .await
        .unwrap();

    let rows = trust.lookup(&host, addr.port()).await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "同 host:port 两次 record 异 key 后必须恰剩一行（R30 单行不变式）"
    );
    assert_eq!(
        rows[0].key_blob,
        String::from_utf8_lossy(k2).into_owned(),
        "存行须为后记录的新键，真实密钥必须已退库"
    );
}

/// M4a 属性对话框的引擎语义，对**真实 OpenSSH sftp-server** 验证。
///
/// 三条判据，各自对应一种「界面看起来正常但信息是错的」的失败：
/// ① 软链的 `meta` 必须是链自身（size = 链文本长度，不是目标的大小）；
/// ② `link_target` 是 readlink 原文（相对链照原样，不替用户解析）；
/// ③ 断链的 `target_meta` 是 None 而不是 Err —— 「链在、目标不可达」是状态不是错误。
#[tokio::test(flavor = "multi_thread")]
async fn describe_entry_separates_link_from_its_target() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, sftp) = open_sftp("sftpdesc").await;
    use fs_sshengine::sftp::{describe_entry, FileType};

    // 目标文件 8 字节；链名指向它。链自身的 size 是链文本长度（"payload."→8 的巧合要避开，
    // 故目标内容取 12 字节、链文本 8 字节，两个数字不同才能分辨拿到的是哪一个）
    sftp.write_at("payload.bin", 0, b"0123456789ab")
        .await
        .unwrap();
    sftp.symlink("payload.bin", "cur.lnk").await.unwrap();

    let d = describe_entry(&sftp, "cur.lnk").await.unwrap();
    assert_eq!(
        d.meta.file_type,
        FileType::Symlink,
        "属性须取 lstat（链自身）"
    );
    assert_eq!(
        d.meta.size, 11,
        "链自身的 size 应为链文本长度（\"payload.bin\" = 11）；若为 12 说明拿的是目标的属性"
    );
    assert_eq!(
        d.link_target.as_deref(),
        Some("payload.bin"),
        "readlink 原文"
    );
    let tm = d.target_meta.expect("有效链必须给出目标属性");
    assert_eq!(tm.file_type, FileType::Regular);
    assert_eq!(tm.size, 12, "目标属性须来自 stat（跟随链）");

    // 相对链原样保留（不替用户解析成绝对路径——解析后断链会显示成有效链）
    sftp.mkdir("sub").await.unwrap();
    sftp.symlink("../payload.bin", "sub/rel.lnk").await.unwrap();
    let rel = describe_entry(&sftp, "sub/rel.lnk").await.unwrap();
    assert_eq!(rel.link_target.as_deref(), Some("../payload.bin"));
    assert!(rel.target_meta.is_some(), "相对链在其所在目录下有效");

    // 断链：target_meta = None，且**不是** Err
    sftp.symlink("nope-does-not-exist", "broken.lnk")
        .await
        .unwrap();
    let b = describe_entry(&sftp, "broken.lnk").await.unwrap();
    assert_eq!(b.meta.file_type, FileType::Symlink);
    assert_eq!(b.link_target.as_deref(), Some("nope-does-not-exist"));
    assert!(
        b.target_meta.is_none(),
        "断链的目标属性须为 None（界面显示「目标不可访问」），实得 {:?}",
        b.target_meta
    );

    // 普通文件：后两栏不问服务端，恒 None
    let f = describe_entry(&sftp, "payload.bin").await.unwrap();
    assert_eq!(f.meta.file_type, FileType::Regular);
    assert!(f.link_target.is_none() && f.target_meta.is_none());
    assert_eq!(f.meta.size, 12);
    // 真实 sftp-server 会回权限位；「未知」在这里不该出现（否则前端的「—」分支会掩盖真缺陷）
    assert!(f.meta.mode.is_some(), "OpenSSH sftp-server 必回 mode");

    // 目录
    let dir = describe_entry(&sftp, "sub").await.unwrap();
    assert_eq!(dir.meta.file_type, FileType::Dir);

    // 不存在的路径：Err（属性对话框据此在窗内显示错误，而不是显示一份空属性）
    assert!(describe_entry(&sftp, "no-such-thing").await.is_err());

    // 清理
    sftp.remove("cur.lnk").await.unwrap();
    sftp.remove("broken.lnk").await.unwrap();
    sftp.remove("sub/rel.lnk").await.unwrap();
    sftp.remove_dir("sub").await.unwrap();
    sftp.remove("payload.bin").await.unwrap();
}

/// 软链参数序的**非空证明**（已有 `sftp_crud_over_real_sshd` 覆盖一次，这里从命令层
/// 的口径再钉一次）：`symlink(target, link_path)` 之后，被创建的条目是 `link_path`，
/// 且 `link_path` 自身是软链、`target` 仍是原类型。参数写反会在两处同时暴露：
/// `lstat(link_path)` 报「不存在」，而 `lstat(target)` 变成软链。
#[tokio::test(flavor = "multi_thread")]
async fn symlink_argument_order_creates_the_link_at_link_path() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, sftp) = open_sftp("sftpsym").await;
    use fs_sshengine::sftp::FileType;
    sftp.write_at("real.txt", 0, b"x").await.unwrap();
    sftp.symlink("real.txt", "alias.txt").await.unwrap();

    let link = sftp.lstat("alias.txt").await.unwrap();
    assert_eq!(
        link.file_type,
        FileType::Symlink,
        "被创建的条目应是 alias.txt"
    );
    let real = sftp.lstat("real.txt").await.unwrap();
    assert_eq!(
        real.file_type,
        FileType::Regular,
        "target 必须保持原类型；若它变成了软链，说明两参写反了"
    );
    sftp.remove("alias.txt").await.unwrap();
    sftp.remove("real.txt").await.unwrap();
}

/// M4a 文件夹递归枚举，对**真实 OpenSSH sftp-server** 验证。
///
/// 单测用内存 fake 钉了守卫与过滤（截断/深度/条数、软链跳过），但内存 fake 的目录
/// 类型标记是我自己填的。真服务器上要验的是**另一件事**：READDIR 回来的类型判定
/// （is_dir/is_symlink 来自 LSTAT 语义的属性）在真实实现下确实能把「目录里的软链」
/// 与「真目录」分开——搞错这一条，一个指向 `/` 的链会让枚举试图递归整台机器。
#[tokio::test(flavor = "multi_thread")]
async fn walk_files_over_real_sshd_separates_dirs_links_and_files() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, sftp) = open_sftp("sftpwalk").await;
    use fs_sshengine::filter::ExcludeFilter;
    use fs_sshengine::sftp::walk_files;

    // 构造：tree/{a.txt, sub/{b.txt}, dirlink -> sub, filelink -> a.txt, skip.tmp}
    sftp.mkdir("tree").await.unwrap();
    sftp.mkdir("tree/sub").await.unwrap();
    sftp.write_at("tree/a.txt", 0, b"A").await.unwrap();
    sftp.write_at("tree/sub/b.txt", 0, b"B").await.unwrap();
    sftp.write_at("tree/skip.tmp", 0, b"T").await.unwrap();
    sftp.symlink("sub", "tree/dirlink").await.unwrap();
    sftp.symlink("a.txt", "tree/filelink").await.unwrap();

    let mut r = walk_files(&sftp, "tree", &ExcludeFilter::default())
        .await
        .unwrap();
    r.files.sort();
    r.skipped_links.sort();
    assert_eq!(
        r.files,
        vec!["a.txt".to_string(), "skip.tmp".into(), "sub/b.txt".into()],
        "只出普通文件，且相对根"
    );
    assert_eq!(
        r.skipped_links,
        vec!["dirlink".to_string(), "filelink".into()],
        "两种软链都必须被跳过并如实上报——指向目录的那个尤其要紧：跟随它会递归到目录之外"
    );
    assert_eq!(
        r.dirs,
        vec!["sub".to_string()],
        "子目录清单供上传方向先建目录"
    );

    // 过滤器在真实列表上同样生效（与面板共用同一个匹配器）
    let f = ExcludeFilter::parse("*.tmp;sub/");
    let r2 = walk_files(&sftp, "tree", &f).await.unwrap();
    assert_eq!(r2.files, vec!["a.txt".to_string()]);
    assert!(r2.dirs.is_empty(), "被排除的目录不该出现在待建目录清单里");

    // 清理
    sftp.remove("tree/dirlink").await.unwrap();
    sftp.remove("tree/filelink").await.unwrap();
    fs_sshengine::sftp::remove_tree(&sftp, "tree")
        .await
        .unwrap();
}

/// M4a 外部编辑器回传的**截断语义**，对真实服务器验证。
///
/// 回传走 `truncate(0) + write_at(0)`。少了 truncate 这一步，把一个长文件改短后回传
/// 只会盖住前半段、旧尾巴留在远端——那是一个语法上合法、语义上错误的配置文件，
/// 而且没有任何报错。这条只能在真实服务器上验：内存 fake 的 write_at 未实现。
#[tokio::test(flavor = "multi_thread")]
async fn editor_writeback_truncates_before_writing() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, sftp) = open_sftp("sftpedit").await;
    // 远端原文件较长
    let long = b"# old config with a very long tail that must not survive\n";
    sftp.write_at("edit.conf", 0, long).await.unwrap();
    // 用户在编辑器里把它改短
    let short = b"# short\n";
    sftp.truncate("edit.conf", 0).await.unwrap();
    sftp.write_at("edit.conf", 0, short).await.unwrap();

    let got = sftp.read_range("edit.conf", 0, 4096).await.unwrap();
    assert_eq!(
        got, short,
        "回传后远端内容必须与本地逐字节相同；若长于本地，说明少了 truncate 而旧尾巴留在了远端"
    );
    let meta = sftp.stat_meta("edit.conf").await.unwrap();
    assert_eq!(meta.size, short.len() as u64, "size 也必须缩到新长度");
    sftp.remove("edit.conf").await.unwrap();
}

/// 回归：`truncate` 必须**只**发 size 属性，且返回 Ok。
///
/// russh-sftp 2.3.0 的 `FileAttributes::default()` 不是全 None，而是
/// `size:0, uid:0, gid:0, permissions:0o777|S_IFDIR, atime:0, mtime:0`。写成
/// `FileAttributes { size: Some(n), ..Default::default() }` 会让 SETSTAT 顺带请求
/// chown-to-root / chmod-0777-目录位 / 时间戳清零；真实 sftp-server 上的表现是
/// **尺寸改成功了、调用却回 Permission denied**（先应用 size、再在 chown 处失败）。
///
/// 这种「做了却报错」最坏：上层要么按错误重试、要么把它当致命失败，而磁盘状态已经变了。
/// 生产路径里 `write_remote_identity`（.fspart 身份记录）与非续传上传的清零都走这条，
/// 之前一直在吃这个假错误。
///
/// 判据必须同时钉**返回值**与**副作用**：只断言尺寸变了的话，回归到 `..Default::default()`
/// 版本照旧全绿——那正是缺陷存在期间的样子。属主/权限/时间戳也一并检查，确保没被顺手改掉。
#[tokio::test(flavor = "multi_thread")]
async fn truncate_sends_only_size_and_reports_success() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, sftp) = open_sftp("sftptrunc").await;
    sftp.write_at("t2.bin", 0, b"0123456789").await.unwrap();
    let before = sftp.lstat("t2.bin").await.unwrap();

    // 缩短：必须 Ok（缺陷版在这里回 Permission denied）
    sftp.truncate("t2.bin", 4)
        .await
        .expect("truncate 必须成功——若报 Permission denied，说明 SETSTAT 顺带发了 uid/gid/perm");
    assert_eq!(sftp.stat_size("t2.bin").await.unwrap(), 4);

    // 放大（SETSTAT(size) 的另一半语义）同样必须 Ok
    sftp.truncate("t2.bin", 32).await.unwrap();
    assert_eq!(sftp.stat_size("t2.bin").await.unwrap(), 32);

    // 属主/权限/时间戳不得被顺手改掉：缺陷版请求的正是 chown 0:0 + chmod 0o777|S_IFDIR + mtime 0
    let after = sftp.lstat("t2.bin").await.unwrap();
    assert_eq!(after.mode, before.mode, "truncate 不得改权限位");
    assert_eq!(after.uid, before.uid, "truncate 不得改属主");
    assert_eq!(after.gid, before.gid, "truncate 不得改属组");
    assert_ne!(after.mtime, 0, "truncate 不得把 mtime 清成 1970");
    assert_eq!(
        after.file_type,
        fs_sshengine::sftp::FileType::Regular,
        "truncate 不得把普通文件的类型位改成目录"
    );
    sftp.remove("t2.bin").await.unwrap();
}
