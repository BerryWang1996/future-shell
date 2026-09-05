//! 并发首启时，两条 `open_or_create` 不得各铸一把主密钥。
//!
//! 缺陷形态：「查 keyring → `NoEntry` → 铸新钥 → `set_password`」若不是原子的，
//! 两条首启路径会各铸一把，凭据库里只剩后写的那把，先写者手里的 `Store` 却仍拿自己
//! 那把封记录。这一步**当场无感**：那次会话里读写都用同一个 `Store`，一切正常；
//! 直到下一次启动走 `unlock_with_keyring`（app 的默认静默解锁路径）才暴露 ——
//! 而此时封了记录的密钥根本没被保存到任何地方，凭据永久不可解。
//!
//! 现象本身在本仓库出现过（`tests/common/mod.rs` 头注记录的 aad_binding 随机 `Integrity`），
//! 但当时的处置是在测试装置里预先把条目铸好、令所有测试一律走「条目已存在」分支。
//! 那是把测试的噪声消掉，不是把产品的窗口关掉；本文件补上后者的回归。
//!
//! 可达路径：`vault_init` 与「库文件尚不存在时的 `vault_unlock`」都会落到 `open_or_create`
//!（见 app/src/commands/vault_cmd.rs），两条 IPC 命令之间没有任何互斥。
//!
//! ## 为什么不用 `tests/common` 的装置
//!
//! 那个装置的 `Once` 会先建一次库把条目铸出来 —— 正好把本测试要复现的分支绕过去。
//! 本文件自带一个凭据库实现，且**只放这一个测试**：`set_default_store` 是进程全局开关，
//! 同一二进制里再有别的测试就会互相看见对方的凭据库。
//!
//! ## 交错为什么是确定的（不是靠 sleep 撞运气）
//!
//! 竞态测试最容易写成「多跑几次总能撞上」的形态，那种测试在 CI 上既会漏报也会假红。
//! 这里把交错点做进凭据库自己：**第一个**读到空条目的线程会先发一声「窗口开了」，
//! 然后在库内部按住 `HOLD` 不返回；主线程收到这声通知才开始自己那次 `open_or_create`。
//! 于是：
//!   · 修复前 —— 主线程作为第二个观察者立刻也读到空，抢在后台线程醒来之前铸钥并写回；
//!     后台线程醒来后再写一次，凭据库终值是**后台线程**那把，主线程手里的 `Store` 就此错配。
//!     交错由 `HOLD` 与「先通知后按住」的顺序钉死，不依赖调度运气。
//!   · 修复后 —— 「取或铸」整段互斥，主线程在拿到通知后会**堵在锁上**直到后台线程铸完，
//!     再读到的就是同一把密钥。测试照过，只是慢 `HOLD` 那么久。
//! 注意修复后主线程是被锁挡住而不是被条件变量挡住，故这里不能用 `Barrier`：
//! `Barrier` 要求双方都到齐，修复后第二个观察者根本不会出现，测试会挂死而不是变绿。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use keyring_core::api::{CredentialApi, CredentialStoreApi};
use keyring_core::{Credential, CredentialPersistence, Entry, Error as KeyringError, Result};

/// 第一个观察者按住空条目的时长。只需明显长于「另一个线程铸钥并写回」所需的时间
///（几次 base64/随机数，微秒量级），500ms 有三个数量级的余量。
const HOLD: Duration = Duration::from_millis(500);

// ── 一个会把竞态窗口撑开的凭据库 ────────────────────────────────────────────────

struct Window {
    /// 已经有人进过窗口了吗。第二个及以后的观察者一律直接放行 —— 让它们抢在第一个前面写回，
    /// 从而把「后写者赢」这件事变成确定的。
    opened: Mutex<bool>,
    tell: SyncSender<()>,
}

impl Window {
    fn first_observer_holds(&self) {
        let mut opened = self.opened.lock().unwrap_or_else(|p| p.into_inner());
        if *opened {
            return;
        }
        *opened = true;
        drop(opened); // 按住期间不占这把锁，否则第二个观察者会跟着一起被挡住
        let _ = self.tell.try_send(());
        std::thread::sleep(HOLD);
    }
}

struct RacyCred {
    spec: (String, String),
    data: Mutex<Option<Vec<u8>>>,
    window: Arc<Window>,
}

impl CredentialApi for RacyCred {
    fn set_secret(&self, secret: &[u8]) -> Result<()> {
        *self.data.lock().unwrap_or_else(|p| p.into_inner()) = Some(secret.to_vec());
        Ok(())
    }

    fn get_secret(&self) -> Result<Vec<u8>> {
        // 先把值取出来再放锁：按住窗口时不能占着条目锁，否则挡住的是所有人，窗口就撑不开了。
        let current = self.data.lock().unwrap_or_else(|p| p.into_inner()).clone();
        match current {
            Some(v) => Ok(v),
            None => {
                self.window.first_observer_holds();
                Err(KeyringError::NoEntry)
            }
        }
    }

    fn delete_credential(&self) -> Result<()> {
        match self.data.lock().unwrap_or_else(|p| p.into_inner()).take() {
            Some(_) => Ok(()),
            None => Err(KeyringError::NoEntry),
        }
    }

    fn get_credential(&self) -> Result<Option<Arc<Credential>>> {
        match *self.data.lock().unwrap_or_else(|p| p.into_inner()) {
            Some(_) => Ok(None),
            None => Err(KeyringError::NoEntry),
        }
    }

    fn get_specifiers(&self) -> Option<(String, String)> {
        Some(self.spec.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

struct RacyStore {
    creds: Mutex<Vec<Arc<RacyCred>>>,
    window: Arc<Window>,
}

impl CredentialStoreApi for RacyStore {
    fn vendor(&self) -> String {
        "fs_vault test store: holds the window open on the first empty read".into()
    }

    fn id(&self) -> String {
        "keyring_mint_race".into()
    }

    fn build(
        &self,
        service: &str,
        user: &str,
        mods: Option<&HashMap<&str, &str>>,
    ) -> Result<Entry> {
        assert!(
            mods.is_none_or(|m| m.is_empty()),
            "被测代码不该给条目带修饰符；带了说明产品侧换了用法，本装置需要同步"
        );
        let mut creds = self.creds.lock().unwrap_or_else(|p| p.into_inner());
        // 同一 (service, user) 必须交回**同一个**条目：产品代码每次都现建 `Entry`，
        // 若这里各给一份独立存储，写进去的密钥立刻就找不回来了。
        if let Some(c) = creds
            .iter()
            .find(|c| c.spec.0 == service && c.spec.1 == user)
        {
            return Ok(Entry::new_with_credential(c.clone()));
        }
        let cred = Arc::new(RacyCred {
            spec: (service.into(), user.into()),
            data: Mutex::new(None),
            window: self.window.clone(),
        });
        creds.push(cred.clone());
        Ok(Entry::new_with_credential(cred))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn persistence(&self) -> CredentialPersistence {
        CredentialPersistence::ProcessOnly
    }
}

/// 装库并交回「窗口已开」的通知端。
///
/// 顺序与 `tests/common/mod.rs` 一致：keyring 4.x 的兼容层会在**首次** `Entry::new` 时
/// 一次性装入平台凭据库并覆盖预设默认库，故必须先触发一次再装自己的，
/// 否则本测试会去读写用户真实的系统凭据库。
fn install() -> Receiver<()> {
    // 本文件只允许有一个测试走到这里。第二个测试会把第一个的凭据库连同窗口一起换掉，
    // 而窗口是「一进程一次」的，换掉之后谁也撑不开它 —— 那就退化成偶发假绿了。
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    assert!(
        !INSTALLED.swap(true, Ordering::SeqCst),
        "set_default_store 是进程全局开关：本文件不得再加第二个测试"
    );
    let (tell, hear) = sync_channel(1);
    let _ = keyring::Entry::new("future-shell-test-trigger", "init");
    keyring_core::set_default_store(Arc::new(RacyStore {
        creds: Mutex::new(Vec::new()),
        window: Arc::new(Window {
            opened: Mutex::new(false),
            tell,
        }),
    }));
    hear
}

/// 每次调用返回一个全新的空目录（口径同 `tests/common::tempdir`，此处不引入那个模块 ——
/// 见文件头注：它的 `Once` 会把本测试要复现的分支预先绕过去）。
fn tempdir(prefix: &str) -> std::path::PathBuf {
    static C: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    loop {
        let p = std::env::temp_dir().join(format!(
            "fs-vault-{prefix}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时钟早于 UNIX 纪元")
                .as_nanos(),
            C.fetch_add(1, Ordering::SeqCst)
        ));
        match std::fs::create_dir(&p) {
            Ok(()) => return p,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("创建测试临时目录失败 {}: {e}", p.display()),
        }
    }
}

/// 两条首启路径并发时，凭据库里最终那把主密钥必须就是各自 `Store` 手里的那把。
///
/// 断言不去比对密钥本身（`Store` 不暴露它，也不该暴露），而是走用户能看见的那条路：
/// 存一条凭据 → 关掉 → 按 app 的默认路径（keyring）重新解锁 → 必须还能读出原文。
/// 这正是「下次启动」的最小复现，也是错配唯一会暴露的地方。
///
/// 两个线程刻意用**不同的数据目录**：错配的根源是凭据库条目全局唯一（`(SERVICE, USER)`
/// 与目录无关），而 `VaultGuard` 是按目录分的。用不同目录既排除了落盘闸门/世代号
/// 那条已被修好的路径的干扰（两次 `save` 各写各的文件，谁都不会被世代比对拒绝），
/// 也顺带钉住了「这把互斥不能挂在 `VaultGuard` 上」这个设计约束 ——
/// 真挂上去的话，本测试会当场变红。
#[test]
fn concurrent_first_runs_must_agree_on_one_master_key() {
    let hear = install();

    let held = tempdir("mint-held");
    let holder = std::thread::spawn({
        let held = held.clone();
        move || fs_vault::Store::open_or_create(&held, None).map(|_| ())
    });

    // 等第一个观察者进窗口。等不到就说明装置没被走到（比如产品代码改成不查 keyring 了），
    // 那必须报错，而不是让测试在「谁也没并发」的情况下轻松变绿。
    hear.recv_timeout(Duration::from_secs(10))
        .expect("没有任何线程读到空条目：本测试的前提（首启会铸钥）已不成立，装置需要重写");

    let raced = tempdir("mint-raced");
    let mut store = fs_vault::Store::open_or_create(&raced, None)
        .expect("窗口期内的首启不该失败：它与另一个目录无任何共享状态");

    holder
        .join()
        .expect("首启线程 panic")
        .expect("按住窗口的那次首启不该失败");

    // 此刻凭据库里是最后一次写回的那把。修复前那是 holder 铸的，与 `store` 手里的不是同一把。
    let secret = b"correct horse battery staple".to_vec();
    let id = store
        .put(
            fs_vault::SecretKind::Password,
            "raced".into(),
            zeroize::Zeroizing::new(secret.clone()),
        )
        .expect("落盘失败");
    drop(store);

    let reopened = fs_vault::Store::unlock_with_keyring(&raced).unwrap_or_else(|e| {
        panic!("按 app 的默认路径重新解锁失败：{e}（凭据库里的主密钥与封记录的那把不是同一把）")
    });
    let got = reopened.get(id).unwrap_or_else(|e| {
        panic!("重新解锁后读不出刚存的凭据：{e}（并发首启各铸了一把主密钥，密文已永久不可解）")
    });
    assert_eq!(
        got.as_slice(),
        secret.as_slice(),
        "重新解锁后读出的不是存进去的内容"
    );
}
