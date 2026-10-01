//! 远程 I/O 的时间边界与超时后的退出语义（审计2 #11 / #25）。
//!
//! 全部用例跑在 `#[tokio::test(start_paused = true)]` 的虚拟时钟上：预算是分钟级的，真实
//! 时钟下这一份文件要跑一个多小时。虚拟时钟不是「把超时调小以便测试」——被测的仍然是
//! 生产常量本身（`CONTROL_TIMEOUT` / `DATA_TIMEOUT` / `EXEC_TOTAL_TIMEOUT`），只是时间
//! 走得快。把常量改小来迁就测试则会让用例对**真实**预算失去约束力。
use bytes::Bytes;
use fs_sshengine::sftp::{Entry, FileMeta, FileType, RemoteReader, RemoteWriter, SftpOps};
use fs_sshengine::timeouts::{
    TimedExec, TimedSftp, CONTROL_TIMEOUT, DATA_TIMEOUT, EXEC_HARD_TIMEOUT, EXEC_TOTAL_TIMEOUT,
};
use fs_sshengine::verify::{run_exec_channel, ChannelMsgs, ExecChannel};
use fs_sshengine::Error;
use russh::ChannelMsg;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ───────────────────────────── 假对端 ─────────────────────────────

#[derive(Clone, Copy, Debug)]
enum Mode {
    /// 三次握手成功、认证成功，随后再不回话——本次要修的就是这种对端。
    Hang,
    /// 立刻回话。
    Ready,
    /// 慢，但会回话。用来分辨「哪个方法走的是哪档预算」。
    Slow(Duration),
}

/// 一个把 14 个 `SftpOps` 方法全实现掉的假对端：统一记账、统一按 `Mode` 决定要不要回话。
///
/// 返回值刻意由入参拼出来（而不是常量），`last` 又把每次调用的实参原样记下来：装饰器里那
/// 14 行转发是**逐行手写**的，最容易出的错不是漏了超时，而是把 `symlink(target, link)`
/// 转发成 `symlink(link, target)`、把 `read_range` 的 offset 和 len 写反——这类错误编译
/// 通过、类型正确、超时也照常生效，只有比对实参才看得见。
struct Fake {
    mode: Mutex<Mode>,
    calls: AtomicUsize,
    last: Mutex<String>,
}

impl Fake {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode: Mutex::new(mode),
            calls: AtomicUsize::new(0),
            last: Mutex::new(String::new()),
        })
    }
    fn set(&self, m: Mode) {
        *self.mode.lock().unwrap() = m;
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn last(&self) -> String {
        self.last.lock().unwrap().clone()
    }
    /// 每个方法的唯一入口。锁在 await 之前就放掉（`Mode: Copy`），免得假对端自己成为死锁源。
    async fn enter(&self, note: String) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().unwrap() = note;
        let mode = *self.mode.lock().unwrap();
        match mode {
            Mode::Hang => std::future::pending::<()>().await,
            Mode::Ready => {}
            Mode::Slow(d) => tokio::time::sleep(d).await,
        }
    }
}

struct FakeReader<'a> {
    fake: &'a Fake,
    path: &'a str,
}

#[async_trait::async_trait]
impl RemoteReader for FakeReader<'_> {
    async fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        self.fake
            .enter(format!("reader_read {} {offset} {len}", self.path))
            .await;
        Ok(format!("r|{}|{offset}|{len}", self.path).into_bytes())
    }
}

struct FakeWriter<'a> {
    fake: &'a Fake,
    path: &'a str,
    queued: u64,
}

#[async_trait::async_trait]
impl RemoteWriter for FakeWriter<'_> {
    async fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        self.fake
            .enter(format!(
                "writer_write {} {} {}",
                self.path,
                self.queued,
                data.len()
            ))
            .await;
        self.queued += data.len() as u64;
        Ok(())
    }
    /// 故意与 `queued` 不同：装饰器必须原样转交内层的「已确认」，而不是自己另算一个。
    fn confirmed(&self) -> u64 {
        self.queued / 2
    }
    async fn finish(self: Box<Self>) -> Result<(), Error> {
        self.fake
            .enter(format!("writer_finish {} {}", self.path, self.queued))
            .await;
        Ok(())
    }
}

#[async_trait::async_trait]
impl SftpOps for Fake {
    async fn list(&self, path: &str) -> Result<fs_sshengine::sftp::ListResult, Error> {
        self.enter(format!("list {path}")).await;
        Ok(fs_sshengine::sftp::ListResult {
            entries: vec![Entry {
                name: path.to_string(),
                is_dir: false,
                is_symlink: false,
                size: 1,
                mtime: 2,
                perms: None,
            }],
            truncated: false,
        })
    }
    async fn stat_size(&self, path: &str) -> Result<u64, Error> {
        self.enter(format!("stat_size {path}")).await;
        Ok(4242)
    }
    async fn stat_meta(&self, path: &str) -> Result<FileMeta, Error> {
        self.enter(format!("stat_meta {path}")).await;
        Ok(meta(path.len() as u64))
    }
    async fn read_range(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        self.enter(format!("read_range {path} {offset} {len}"))
            .await;
        Ok(format!("{path}|{offset}|{len}").into_bytes())
    }
    async fn write_at(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        self.enter(format!("write_at {path} {offset} {}", data.len()))
            .await;
        Ok(())
    }
    async fn sync(&self, path: &str) -> Result<(), Error> {
        self.enter(format!("sync {path}")).await;
        Ok(())
    }
    /// 覆写而不用默认实现，理由同 `open_writer`：自带的读取器记下 `reader_read`，
    /// 与「装饰器没覆写、退回默认实现经 `read_range` 读」分得开。
    async fn open_reader<'a>(&'a self, path: &'a str) -> Result<Box<dyn RemoteReader + 'a>, Error> {
        self.enter(format!("open_reader {path}")).await;
        Ok(Box::new(FakeReader { fake: self, path }))
    }
    /// 覆写而不用默认实现：默认实现逐块调 `write_at`，于是「装饰器把 `open_writer` 转交给了
    /// 内层」和「装饰器没覆写、退回了自己的默认实现（经自己的 `write_at` 逐块写）」在这里
    /// 看起来一模一样——都落到 `write_at`。自带的写入器记下的是 `writer_write`，两者才分得开。
    async fn open_writer<'a>(
        &'a self,
        path: &'a str,
        offset: u64,
    ) -> Result<Box<dyn RemoteWriter + 'a>, Error> {
        self.enter(format!("open_writer {path} {offset}")).await;
        Ok(Box::new(FakeWriter {
            fake: self,
            path,
            queued: offset,
        }))
    }
    async fn truncate(&self, path: &str, size: u64) -> Result<(), Error> {
        self.enter(format!("truncate {path} {size}")).await;
        Ok(())
    }
    async fn mkdir(&self, path: &str) -> Result<(), Error> {
        self.enter(format!("mkdir {path}")).await;
        Ok(())
    }
    async fn remove(&self, path: &str) -> Result<(), Error> {
        self.enter(format!("remove {path}")).await;
        Ok(())
    }
    async fn remove_dir(&self, path: &str) -> Result<(), Error> {
        self.enter(format!("remove_dir {path}")).await;
        Ok(())
    }
    async fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        self.enter(format!("rename {from} {to}")).await;
        Ok(())
    }
    async fn lstat(&self, path: &str) -> Result<FileMeta, Error> {
        self.enter(format!("lstat {path}")).await;
        Ok(meta(7))
    }
    async fn read_link(&self, path: &str) -> Result<String, Error> {
        self.enter(format!("read_link {path}")).await;
        Ok(format!("target-of-{path}"))
    }
    async fn symlink(&self, target: &str, link_path: &str) -> Result<(), Error> {
        self.enter(format!("symlink {target} {link_path}")).await;
        Ok(())
    }
    async fn canonicalize(&self, path: &str) -> Result<String, Error> {
        self.enter(format!("canonicalize {path}")).await;
        Ok(path.to_string())
    }
}

fn meta(size: u64) -> FileMeta {
    FileMeta {
        file_type: FileType::Regular,
        size,
        mtime: 0,
        mode: None,
        uid: None,
        gid: None,
    }
}

/// 14 个方法的枚举。之所以列成一张表逐个跑，而不是挑两三个代表：漏掉超时的方式从来
/// 不是「全都漏」，而是「新加的那个忘了套」。表在这里，新增方法而不套 guard 就会缺一行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum M {
    List,
    StatSize,
    StatMeta,
    ReadRange,
    WriteAt,
    Truncate,
    Mkdir,
    Remove,
    Rename,
    Lstat,
    ReadLink,
    RemoveDir,
    Symlink,
    Canonicalize,
}

const ALL: [M; 14] = [
    M::List,
    M::StatSize,
    M::StatMeta,
    M::ReadRange,
    M::WriteAt,
    M::Truncate,
    M::Mkdir,
    M::Remove,
    M::Rename,
    M::Lstat,
    M::ReadLink,
    M::Symlink,
    M::Canonicalize,
    M::RemoveDir,
];

/// 走数据面预算（[`DATA_TIMEOUT`]）的三个方法；其余十一个走控制面（[`CONTROL_TIMEOUT`]）。
const DATA_FACING: [M; 3] = [M::List, M::ReadRange, M::WriteAt];

async fn call(t: &TimedSftp, m: M) -> Result<(), Error> {
    match m {
        M::List => t.list("/p").await.map(|_| ()),
        M::StatSize => t.stat_size("/p").await.map(|_| ()),
        M::StatMeta => t.stat_meta("/p").await.map(|_| ()),
        M::ReadRange => t.read_range("/p", 0, 1).await.map(|_| ()),
        M::WriteAt => t.write_at("/p", 0, b"x").await,
        M::Canonicalize => t.canonicalize("/p").await.map(|_| ()),
        M::Truncate => t.truncate("/p", 0).await,
        M::Mkdir => t.mkdir("/p").await,
        M::Remove => t.remove("/p").await,
        M::RemoveDir => t.remove_dir("/p").await,
        M::Rename => t.rename("/a", "/b").await,
        M::Lstat => t.lstat("/p").await.map(|_| ()),
        M::ReadLink => t.read_link("/p").await.map(|_| ()),
        M::Symlink => t.symlink("/t", "/l").await,
    }
}

// ───────────────────────────── SftpOps 的时间边界 ─────────────────────────────

#[tokio::test(start_paused = true)]
async fn every_sftp_method_is_bounded_and_a_timeout_kills_the_channel() {
    for m in ALL {
        let t = TimedSftp::new(Fake::new(Mode::Hang));
        let e = call(&t, m).await.expect_err("对端从不回话，这一步不该成功");
        assert!(
            matches!(e, Error::Timeout { .. }),
            "{m:?} 没有超时上限，拿到的是：{e}"
        );
        assert!(e.is_timeout(), "{m:?} 的超时没有被 is_timeout 认出来");
        assert!(t.is_poisoned(), "{m:?} 超时之后没有把这条通道判死");
    }
}

#[tokio::test(start_paused = true)]
async fn each_method_sits_on_its_intended_budget() {
    // 一次 60 秒的往返：数据面（120 s）该放行，控制面（30 s）该判超时。
    //
    // 逐个方法钉死而不是抽查，是因为「哪个方法算数据面」是 `timeouts.rs` 里 14 行各写各的
    // **手写映射**，而写错的后果两头都是静默的：把 read_range 错配成控制面，慢链路上传大
    // 文件会毫无理由地中途失败；把 stat 错配成数据面，一次挂死要多冻住 90 秒。
    for m in ALL {
        let t = TimedSftp::new(Fake::new(Mode::Slow(Duration::from_secs(60))));
        let ok = call(&t, m).await.is_ok();
        assert_eq!(
            ok,
            DATA_FACING.contains(&m),
            "{m:?} 的预算档位不对：60 秒往返的结果 ok={ok}，\
             而数据面预算 {}s、控制面预算 {}s",
            DATA_TIMEOUT.as_secs(),
            CONTROL_TIMEOUT.as_secs()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_poisoned_channel_fails_fast_without_touching_the_wire() {
    let fake = Fake::new(Mode::Hang);
    let t = TimedSftp::new(fake.clone());
    assert!(call(&t, M::StatSize).await.is_err());
    assert_eq!(fake.calls(), 1);

    // 对端此刻「恢复」了。这不足以让我们继续用这条通道：超时只是把 future 丢掉，那条
    // 已经发出去的请求可能仍在服务端排队、也可能已经生效，通道上的状态我们并不知道。
    fake.set(Mode::Ready);
    for m in ALL {
        let e = call(&t, m)
            .await
            .expect_err("通道已判死，这一步不该有成功的可能");
        assert!(
            matches!(e, Error::Poisoned(_)),
            "{m:?} 在判死之后拿到的不是 Poisoned，而是：{e}"
        );
        assert!(e.is_timeout(), "Poisoned 应当被 is_timeout 认作同一类故障");
    }
    assert_eq!(
        fake.calls(),
        1,
        "判死之后仍然把请求发上了线——退出语义没有生效"
    );
}

#[tokio::test(start_paused = true)]
async fn the_poison_message_names_the_op_and_tells_the_user_what_to_do() {
    let t = TimedSftp::new(Fake::new(Mode::Hang));
    assert!(call(&t, M::StatSize).await.is_err());
    let msg = call(&t, M::Rename).await.unwrap_err().to_string();
    assert!(msg.contains("rename"), "错误没点名是哪个操作被挡下：{msg}");
    assert!(msg.contains("重连"), "错误没告诉用户怎么恢复：{msg}");
}

#[tokio::test(start_paused = true)]
async fn one_channels_death_does_not_touch_another() {
    // 判死标志是**每通道**的：同一台服务器上另一条会话的通道不该被连坐。
    let dead = TimedSftp::new(Fake::new(Mode::Hang));
    let alive = TimedSftp::new(Fake::new(Mode::Ready));
    assert!(call(&dead, M::StatSize).await.is_err());
    assert!(dead.is_poisoned());
    assert!(!alive.is_poisoned());
    assert!(call(&alive, M::StatSize).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn a_healthy_channel_forwards_arguments_and_results_verbatim() {
    let fake = Fake::new(Mode::Ready);
    let t = TimedSftp::new(fake.clone());

    assert_eq!(t.stat_size("/a").await.unwrap(), 4242);
    assert_eq!(fake.last(), "stat_size /a");

    // 读的三个实参必须按原序到达对端：offset 与 len 写反是这一层最容易犯、也最难看见的错。
    assert_eq!(
        t.read_range("/f", 262144, 4096).await.unwrap(),
        b"/f|262144|4096".to_vec()
    );
    assert_eq!(fake.last(), "read_range /f 262144 4096");

    t.write_at("/f", 99, b"abcd").await.unwrap();
    assert_eq!(fake.last(), "write_at /f 99 4");

    // rename 与 symlink 的两个字符串参数同类型、顺序反了照样编译。
    t.rename("/from", "/to").await.unwrap();
    assert_eq!(fake.last(), "rename /from /to");
    t.symlink("/target", "/link").await.unwrap();
    assert_eq!(fake.last(), "symlink /target /link");

    t.truncate("/f", 512).await.unwrap();
    assert_eq!(fake.last(), "truncate /f 512");

    let listed = t.list("/d").await.unwrap();
    assert_eq!(listed.entries[0].name, "/d");
    assert!(!listed.truncated);
    assert_eq!(t.read_link("/l").await.unwrap(), "target-of-/l");
    assert_eq!(t.stat_meta("/abc").await.unwrap().size, 4);
    assert_eq!(t.lstat("/x").await.unwrap().size, 7);
    t.mkdir("/d2").await.unwrap();
    t.remove("/f2").await.unwrap();
    t.remove_dir("/d3").await.unwrap();
    assert_eq!(fake.last(), "remove_dir /d3");
    assert_eq!(t.canonicalize("/rel").await.unwrap(), "/rel");

    assert_eq!(fake.calls(), 14, "14 个方法应当各自真的到达了对端");
    assert!(!t.is_poisoned(), "全程没有超时，不该判死");
}

/// 装饰器必须把 `open_writer` **转交给内层**（1.0.1）。
///
/// 没覆写时装饰器会用 trait 的默认实现：经自己的 `write_at` 逐块同步写。功能完全正确、
/// 超时也照常生效，所有别的用例都绿——但生产上内层是 `RemoteSftp`，它的流水线写入器
/// 从此不会被调用，1.0.1 的上传提速在生产路径上等于没发生。
#[tokio::test(start_paused = true)]
async fn open_writer_is_handed_to_the_inner_writer_verbatim() {
    let fake = Fake::new(Mode::Ready);
    let t = TimedSftp::new(fake.clone());

    let mut w = t.open_writer("/w", 7).await.unwrap();
    assert_eq!(fake.last(), "open_writer /w 7");
    w.write(b"abcd").await.unwrap();
    assert_eq!(
        fake.last(),
        "writer_write /w 7 4",
        "写没有落到内层的写入器上：装饰器退回了默认的逐块写"
    );
    w.write(b"efghij").await.unwrap();
    assert_eq!(fake.last(), "writer_write /w 11 6");
    assert_eq!(w.confirmed(), 17 / 2, "「已确认」必须原样取内层写入器的");
    w.finish().await.unwrap();
    assert_eq!(fake.last(), "writer_finish /w 17");
    assert_eq!(fake.calls(), 4);
    assert!(!t.is_poisoned());
}

/// 装饰器必须把 `open_reader` **转交给内层**（1.0.1），读的两个实参按原序到达。
///
/// 没覆写时退回默认实现：经本层的 `read_range` 逐块读，每块重新 OPEN——功能对、超时也照常，
/// 但生产上 `RemoteSftp` 的句柄复用从此不会被调用。
#[tokio::test(start_paused = true)]
async fn open_reader_is_handed_to_the_inner_reader_verbatim() {
    let fake = Fake::new(Mode::Ready);
    let t = TimedSftp::new(fake.clone());
    let mut r = t.open_reader("/r").await.unwrap();
    assert_eq!(fake.last(), "open_reader /r");
    assert_eq!(
        r.read_at(262144, 4096).await.unwrap(),
        b"r|/r|262144|4096".to_vec()
    );
    assert_eq!(
        fake.last(),
        "reader_read /r 262144 4096",
        "读没有落到内层的读取器上：装饰器退回了默认的逐块 read_range"
    );
    assert_eq!(fake.calls(), 2);
    assert!(!t.is_poisoned());
}

/// 读取器的 `read_at` 与 `read_range` 同档：数据面预算，超时判死整条通道。
#[tokio::test(start_paused = true)]
async fn a_hung_reader_is_bounded_on_the_data_budget_and_kills_the_channel() {
    let fake = Fake::new(Mode::Slow(Duration::from_secs(60)));
    let t = TimedSftp::new(fake.clone());
    let mut r = t
        .open_reader("/r")
        .await
        .expect("打开读取器按数据面预算，60 s 应放行");
    r.read_at(0, 1).await.expect("读按数据面预算，60 s 应放行");
    assert!(!t.is_poisoned());

    let fake = Fake::new(Mode::Ready);
    let t = TimedSftp::new(fake.clone());
    let mut r = t.open_reader("/r").await.unwrap();
    fake.set(Mode::Hang);
    let e = r.read_at(0, 1).await.expect_err("对端从不回话，读不该成功");
    assert!(
        matches!(e, Error::Timeout { .. }),
        "读取器的 read_at 没有超时上限：{e}"
    );
    assert!(t.is_poisoned(), "读取器超时之后没有把这条通道判死");
}

/// 写入器的 `write` / `finish` 与 `write_at` 同档：数据面预算，超时判死整条通道。
#[tokio::test(start_paused = true)]
async fn a_hung_writer_is_bounded_on_the_data_budget_and_kills_the_channel() {
    // 60 秒一次往返：数据面（120 s）放行
    let fake = Fake::new(Mode::Slow(Duration::from_secs(60)));
    let t = TimedSftp::new(fake.clone());
    let mut w = t
        .open_writer("/w", 0)
        .await
        .expect("打开写入器按数据面预算，60 s 应放行");
    w.write(b"x").await.expect("写按数据面预算，60 s 应放行");
    w.finish().await.expect("收尾按数据面预算，60 s 应放行");
    assert!(!t.is_poisoned());

    // 对端挂死：write 超时并判死通道
    let fake = Fake::new(Mode::Ready);
    let t = TimedSftp::new(fake.clone());
    let mut w = t.open_writer("/w", 0).await.unwrap();
    fake.set(Mode::Hang);
    let e = w.write(b"x").await.expect_err("对端从不回话，写不该成功");
    assert!(
        matches!(e, Error::Timeout { .. }),
        "写入器的 write 没有超时上限：{e}"
    );
    assert!(t.is_poisoned(), "写入器超时之后没有把这条通道判死");

    // finish 同理：收尾时等最后一批确认，对端挂死也必须有上限
    let fake = Fake::new(Mode::Ready);
    let t = TimedSftp::new(fake.clone());
    let w = t.open_writer("/w", 0).await.unwrap();
    fake.set(Mode::Hang);
    let e = w.finish().await.expect_err("对端从不回话，收尾不该成功");
    assert!(
        matches!(e, Error::Timeout { .. }),
        "写入器的 finish 没有超时上限：{e}"
    );
    assert!(t.is_poisoned(), "收尾超时之后没有把这条通道判死");
}

// ───────────────────────────── exec 的时间边界 ─────────────────────────────

struct FakeExec {
    mode: Mutex<Mode>,
    calls: AtomicUsize,
}

impl FakeExec {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode: Mutex::new(mode),
            calls: AtomicUsize::new(0),
        })
    }
    fn set(&self, m: Mode) {
        *self.mode.lock().unwrap() = m;
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl ExecChannel for FakeExec {
    async fn exec_once(&self, cmd: &str) -> Result<fs_sshengine::verify::ExecOutput, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mode = *self.mode.lock().unwrap();
        match mode {
            Mode::Hang => std::future::pending::<()>().await,
            Mode::Ready => {}
            Mode::Slow(d) => tokio::time::sleep(d).await,
        }
        Ok(fs_sshengine::verify::ExecOutput {
            code: Some(0),
            stdout: format!("ran {cmd}"),
            stderr: String::new(),
        })
    }
}

#[tokio::test(start_paused = true)]
async fn exec_is_bounded_but_a_timeout_does_not_kill_the_session() {
    let fake = FakeExec::new(Mode::Hang);
    let t = TimedExec::new(fake.clone());
    let e = t.exec_once("sha256sum /big").await.unwrap_err();
    assert!(matches!(e, Error::Timeout { .. }), "exec 没有上限：{e}");
    assert_eq!(fake.calls(), 1);

    // 与 SFTP 相反，这里**故意**不判死：exec 是一次性通道，自开自关，一次超时说明不了
    // 下一次也会挂；而它的两个消费方（提交前闸门、传后校验）都把 exec 失败当作「少一层
    // 证据」而非「不符」，把整条会话判死会让一次慢哈希连累到与校验无关的操作。
    fake.set(Mode::Ready);
    assert_eq!(t.exec_once("echo hi").await.unwrap().stdout, "ran echo hi");
    assert_eq!(fake.calls(), 2, "上一次超时之后，这一次没有真的发出去");
}

#[tokio::test(start_paused = true)]
async fn the_exec_timeout_message_does_not_leak_the_command_line() {
    // 审计1 P1-20：日志不得含命令文本（口令、令牌、私钥路径、数据库连接串都可能在里面）。
    // 超时文案会进日志，所以它只能点名阶段，不能回显命令。
    let t = TimedExec::new(FakeExec::new(Mode::Hang));
    let msg = t
        .exec_once("mysql -u root -p'hunter2' -e 'select 1' | sha256sum")
        .await
        .unwrap_err()
        .to_string();
    assert!(!msg.contains("hunter2"), "超时文案回显了口令：{msg}");
    assert!(!msg.contains("mysql"), "超时文案回显了命令：{msg}");
    assert!(msg.contains("exec"), "超时文案没点名是哪一段：{msg}");
}

#[tokio::test(start_paused = true)]
async fn the_exec_backstop_sits_outside_the_inner_loop_budget() {
    // 两道边界必须分得开：内层（run_exec_channel）到点能带着已收到的输出正常返回，走的是
    // 「证据缺失 → 降级」这条温和路径；外层到点则是把 future 丢掉、通道状态就此不明。
    // 取同一个值的话，谁先响就成了调度器的运气。
    assert!(
        EXEC_HARD_TIMEOUT > EXEC_TOTAL_TIMEOUT,
        "外层兜底 {}s 不大于内层预算 {}s，两道边界会打架",
        EXEC_HARD_TIMEOUT.as_secs(),
        EXEC_TOTAL_TIMEOUT.as_secs()
    );
    // 一条卡在内层预算与外层兜底之间的命令，由外层收掉。
    let mid = (EXEC_TOTAL_TIMEOUT + EXEC_HARD_TIMEOUT) / 2;
    let t = TimedExec::new(FakeExec::new(Mode::Slow(mid)));
    assert!(
        t.exec_once("x").await.is_ok(),
        "外层兜底把还在预算内的命令误杀了"
    );
    let t = TimedExec::new(FakeExec::new(Mode::Slow(
        EXEC_HARD_TIMEOUT + Duration::from_secs(1),
    )));
    assert!(
        matches!(t.exec_once("x").await, Err(Error::Timeout { .. })),
        "超过外层兜底仍未被收掉"
    );
}

// ───────────────────────── run_exec_channel 的两道边界 ─────────────────────────

/// 脚本化通道：按序吐出预置消息，吐完之后按 `then` 决定是收摊还是永远沉默。
struct Scripted {
    msgs: std::collections::VecDeque<ChannelMsg>,
    /// true = 消息吐完后永不返回（服务端卡死）；false = 返回 None（流正常结束）。
    hang_at_end: bool,
}

impl Scripted {
    fn new(msgs: Vec<ChannelMsg>, hang_at_end: bool) -> Self {
        Self {
            msgs: msgs.into(),
            hang_at_end,
        }
    }
}

#[async_trait::async_trait]
impl ChannelMsgs for Scripted {
    async fn next_msg(&mut self) -> Option<ChannelMsg> {
        match self.msgs.pop_front() {
            Some(m) => Some(m),
            None if self.hang_at_end => std::future::pending().await,
            None => None,
        }
    }
}

fn data(s: &'static str) -> ChannelMsg {
    ChannelMsg::Data {
        data: Bytes::from_static(s.as_bytes()),
    }
}

#[tokio::test(start_paused = true)]
async fn exit_status_sent_after_eof_is_still_collected() {
    // S41 的直接回归。OpenSSH 的发送序是 data… → EOF → exit-status → close：在 `Eof` 处
    // break 会把退出码整条丢掉，`code` 对任何真实服务器恒为 -1，于是 run_verify 的 Up 路径
    // 永远落进降级分支，SHA256 校验在真机上从未真正执行过。
    //
    // 这条缺陷此前只有连真服务器的集成测试才撞得见——`run_exec_channel` 的入参曾是具体的
    // `russh::Channel`，crate 外无从构造。改成按 `ChannelMsgs` 取消息之后它才第一次可测。
    let mut ch = Scripted::new(
        vec![
            data("6dcd4ce2  /tmp/x\n"),
            ChannelMsg::Eof,
            ChannelMsg::ExitStatus { exit_status: 0 },
            ChannelMsg::Close,
        ],
        false,
    );
    let o = run_exec_channel(&mut ch).await;
    let (code, out, err) = (o.code, o.stdout, o.stderr);
    assert_eq!(code, Some(0), "EOF 之后才到的 exit-status 被丢掉了");
    assert_eq!(out, "6dcd4ce2  /tmp/x\n");
    assert_eq!(err, "");
}

#[tokio::test(start_paused = true)]
async fn only_ext_1_counts_as_stderr() {
    let mut ch = Scripted::new(
        vec![
            ChannelMsg::ExtendedData {
                ext: 1,
                data: Bytes::from_static(b"boom"),
            },
            // ext=2 及以上不是 stderr（RFC 4254 §5.2 只定义了 1 = SSH_EXTENDED_DATA_STDERR）。
            ChannelMsg::ExtendedData {
                ext: 2,
                data: Bytes::from_static(b"NOT-STDERR"),
            },
            ChannelMsg::ExitStatus { exit_status: 127 },
            ChannelMsg::Close,
        ],
        false,
    );
    let o = run_exec_channel(&mut ch).await;
    let (code, err) = (o.code, o.stderr);
    assert_eq!(code, Some(127));
    assert_eq!(err, "boom");
}

#[tokio::test(start_paused = true)]
async fn a_command_that_never_finishes_is_cut_off_at_the_total_budget() {
    // 命令跑起来之后通道上一个字节都不会有（`sha256sum` 读完整个文件才输出一行），所以
    // 「首字节」「空闲」都不能当判据，只有总时长可用。不设总时长的实际后果是：一个卡死的
    // 服务端进程能把调用方永久占住，而调用方一路上来是持着会话锁的。
    let mut ch = Scripted::new(vec![data("partial")], true);
    let t0 = tokio::time::Instant::now();
    let r = tokio::time::timeout(EXEC_TOTAL_TIMEOUT * 3, run_exec_channel(&mut ch)).await;
    let o = r.expect("跑过三倍总预算仍未返回：run_exec_channel 没有总超时");
    let (code, out) = (o.code, o.stdout);
    let waited = t0.elapsed();

    assert_eq!(code, None, "超时应保持「证据缺失」，不能伪造一个退出码");
    assert_eq!(out, "partial", "超时不该把已经收到的输出丢掉");
    assert!(
        waited >= EXEC_TOTAL_TIMEOUT,
        "还没到总预算就放弃了：等了 {waited:?}"
    );
    assert!(
        waited < EXEC_TOTAL_TIMEOUT + Duration::from_secs(5),
        "超过总预算很久才放弃：等了 {waited:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_server_that_goes_quiet_after_eof_is_cut_off_at_the_tail_grace() {
    // EOF 之后服务器只剩 exit-status 与 close 两条消息要发，迟迟不发即判异常。这一道比总
    // 预算早得多（10 s vs 600 s），所以它必须**盖过**总预算——两个 deadline 取更早的那个。
    let mut ch = Scripted::new(vec![data("ok\n"), ChannelMsg::Eof], true);
    let t0 = tokio::time::Instant::now();
    let r = tokio::time::timeout(EXEC_TOTAL_TIMEOUT * 3, run_exec_channel(&mut ch)).await;
    let o = r.expect("跑过三倍总预算仍未返回");
    let (code, out) = (o.code, o.stdout);
    let waited = t0.elapsed();

    assert_eq!(code, None);
    assert_eq!(out, "ok\n", "尾部超时不该丢掉 EOF 之前收到的输出");
    assert!(
        waited < EXEC_TOTAL_TIMEOUT,
        "EOF 之后一直等到了总预算（{waited:?}）——尾部宽限窗口没有生效"
    );
    assert!(
        waited >= Duration::from_secs(10),
        "EOF 之后没等满宽限窗口就放弃了（{waited:?}），慢一点的服务器会被误杀"
    );
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_ends_without_close_still_returns() {
    // `wait()` 返回 None = 通道被对端拆了。这条路径不该等任何超时。
    let mut ch = Scripted::new(
        vec![data("x"), ChannelMsg::ExitStatus { exit_status: 3 }],
        false,
    );
    let t0 = tokio::time::Instant::now();
    let o = run_exec_channel(&mut ch).await;
    let (code, out) = (o.code, o.stdout);
    assert_eq!((code, out.as_str()), (Some(3), "x"));
    assert!(t0.elapsed() < Duration::from_secs(1), "流结束了还在等超时");
}
