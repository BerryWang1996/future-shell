//! 尺寸上限的日志写入器（审计2 #32）。
//!
//! `tracing_appender` 的滚动写入器只按时间（日/时/分）分文件，`max_log_files(7)` 限的是
//! **文件个数**，限不住**单个文件的大小**：本程序常驻运行，一天内的日志可以无限长，
//! 磁盘被它吃光的风险与「无限增长」没区别。这里换成自写轮转：
//! 单文件超过上限即把当前内容整份留档为 `.old`（上一代），当前文件清空续写。
//!
//! 实现要点：
//! - 轮转用「复制 + 截断」而不是 rename：Windows 上打开中的文件不能改名；
//! - 留档是 best-effort：留不下只损失历史日志，不打断正在进行的记录；
//! - 上限只挡「已有内容的滚动增长」：单条超过上限的记录照写不误（否则丢掉的恰是
//!   最该留的那条现场），代价是文件可能短暂超过上限一条记录的长度；
//! - 打开时从现有文件长度初始化计数：跨进程重启后不会「忘了」文件已经多大；
//! - 上限是 8 MiB 而非更小，因为日志里还承担着传输/校验的排障现场，小上限会把
//!   刚出问题那几分钟的上下文卷进 .old、再被下一轮覆盖掉。

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 默认上限：8 MiB。正常一天的 info 日志远到不了这个量级；到得了说明有东西在
/// 刷屏，那正是该把它卷进 .old 去查的时刻。
pub const DEFAULT_CAP: u64 = 8 * 1024 * 1024;

/// 旋转标记行：当前文件清空后先写这一行，让人一眼看出文件是被轮转清空的，
/// 而不是「程序刚好从这一刻起没写任何东西」。
const ROTATION_MARKER: &str = "【日志已轮转：上一代留档于 futureshell.log.old】\n";

pub struct SizeCappedFile {
    path: PathBuf,
    cap: u64,
    inner: Mutex<Inner>,
}

struct Inner {
    file: File,
    written: u64,
}

impl SizeCappedFile {
    pub fn open(path: &Path, cap: u64) -> io::Result<Self> {
        let mut opts = std::fs::OpenOptions::new();
        // append 保证写入恒在文件末尾（POSIX O_APPEND / Windows FILE_APPEND_DATA）。
        // 注意**不能**对这个句柄 set_len：Windows 上 SetFileInformationByHandle
        // (FileEndOfFileInfo) 对带 FILE_APPEND_DATA 的句柄返回拒绝访问（本机实测
        // os error 5），截断改由 rotate 里的第二个短命句柄完成。
        opts.create(true).write(true).append(true);
        // 日志可能包含主机名、操作名等现场信息：同机同一账户读它属于正常排查，
        // 跨账户不该读——收紧到 0600（审计2 #31 的同款口径）。
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(path)?;
        // append 句柄的写入恒在文件末尾（OpenOptions 不带 truncate），
        // 计数从现有长度起算即可——长度即「已有多少内容」。
        let written = file.metadata()?.len();
        Ok(Self {
            path: path.to_path_buf(),
            cap,
            inner: Mutex::new(Inner { file, written }),
        })
    }

    fn old_path(&self) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| "futureshell.log".into());
        name.push(".old");
        self.path.with_file_name(name)
    }

    /// 清空当前文件，把此前的全部内容留档为 `.old`。返回前新文件里已写上旋转标记，
    /// `written` 从标记长度重新起算。
    fn rotate(&self, inner: &mut Inner) -> io::Result<()> {
        let _ = std::fs::copy(&self.path, self.old_path()); // best-effort，见模块头
                                                            // 清空经由一个短命截断句柄（见 open 的注记：append 句柄本身截不动）。
                                                            // append 句柄的写仍恒在文件末尾；截断后末尾即 0，标记自然落在文件头。
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        inner.file.write_all(ROTATION_MARKER.as_bytes())?;
        inner.written = ROTATION_MARKER.len() as u64;
        Ok(())
    }
}

impl Write for SizeCappedFile {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        // 锁被毒化 = 上一个持锁者 panic 过。日志线程不值得为这个崩溃，
        // 抢救出文件继续写（into_inner）。
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        // 只有「已有内容」才轮转：written == 0 时直接写，避免一条超过上限的
        // 记录触发无限自转。
        if inner.written > 0 && inner.written + data.len() as u64 > self.cap {
            self.rotate(&mut inner)?;
        }
        inner.file.write_all(data)?;
        inner.written += data.len() as u64;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // 毒化时无从 flush（原文件句柄随 panic 丢失），跳过而不是报错——
        // 日志子系统不该因为 flush 失败而变更自己的记录行为。
        if let Ok(mut inner) = self.inner.lock() {
            inner.file.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("fs-logfile-test-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("读 {} 失败：{e}", path.display()))
    }

    #[test]
    fn writes_under_cap_stay_in_one_file() {
        let dir = scratch("under");
        let path = dir.join("futureshell.log");
        let mut w = SizeCappedFile::open(&path, 1024).unwrap();
        w.write_all(b"hello\n").unwrap();
        w.write_all(b"world\n").unwrap();
        w.flush().unwrap();
        assert_eq!(read(&path), "hello\nworld\n");
        assert!(
            !dir.join("futureshell.log.old").exists(),
            "没到上限不该产生留档"
        );
    }

    #[test]
    fn crossing_the_cap_archives_the_old_content_and_marks_rotation() {
        let dir = scratch("rotate");
        let path = dir.join("futureshell.log");
        let mut w = SizeCappedFile::open(&path, 14).unwrap();
        w.write_all(b"AAAA\n").unwrap(); // 5
        w.write_all(b"BBBB\n").unwrap(); // 10
        w.write_all(b"CCCC\n").unwrap(); // 10 + 5 > 14 → 轮转
        w.flush().unwrap();
        assert_eq!(
            read(&dir.join("futureshell.log.old")),
            "AAAA\nBBBB\n",
            "留档的不是轮转前的完整内容"
        );
        let cur = read(&path);
        assert!(
            cur.starts_with("【日志已轮转"),
            "轮转 = 清空 + 标记落在文件头；旧内容仍在标记之前说明根本没清空：{cur}"
        );
        assert!(
            cur.ends_with("CCCC\n"),
            "当前文件里没有轮转后新写的内容：{cur}"
        );
    }

    #[test]
    fn reopening_counts_existing_bytes_toward_the_cap() {
        let dir = scratch("reopen");
        let path = dir.join("futureshell.log");
        {
            let mut w = SizeCappedFile::open(&path, 64).unwrap();
            w.write_all(&[b'x'; 50]).unwrap();
            w.write_all(b"\n").unwrap(); // 51 字节
        }
        {
            let mut w = SizeCappedFile::open(&path, 64).unwrap();
            // 已有 51 字节必须计入：51 + 20 > 64 → 轮转。
            // 若重开时把计数清零，这里就是静默的「新文件」，上限形同虚设。
            w.write_all(&[b'y'; 20]).unwrap();
            w.flush().unwrap();
        }
        let old = std::fs::read(dir.join("futureshell.log.old")).unwrap();
        assert_eq!(old, [&[b'x'; 50][..], b"\n"].concat());
        let cur = read(&path);
        assert!(
            cur.starts_with("【日志已轮转"),
            "重开后没有轮转（或没清空）：{cur}"
        );
        assert!(cur.ends_with(&"y".repeat(20)));
    }

    #[test]
    fn a_single_oversized_record_is_written_not_spun() {
        let dir = scratch("oversize");
        let path = dir.join("futureshell.log");
        let mut w = SizeCappedFile::open(&path, 64).unwrap();
        // 一条远超上限的记录：宁让文件短暂超限，也不能丢记录或无限自转。
        w.write_all(&[b'z'; 1000]).unwrap();
        w.flush().unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 1000);
        assert!(
            !dir.join("futureshell.log.old").exists(),
            "空文件里写第一条不该触发轮转"
        );
    }

    #[test]
    fn the_current_file_stays_bounded_across_many_rotations() {
        let dir = scratch("bounded");
        let path = dir.join("futureshell.log");
        let mut w = SizeCappedFile::open(&path, 128).unwrap();
        for i in 0..50 {
            w.write_all(format!("line-{i:03}\n").as_bytes()).unwrap();
        }
        w.flush().unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        assert!(
            len <= 128 + 16,
            "轮转多轮后当前文件仍在无限增长：{len} 字节"
        );
        assert!(dir.join("futureshell.log.old").exists());
    }

    #[test]
    fn survives_a_poisoned_lock() {
        let dir = scratch("poison");
        let path = dir.join("futureshell.log");
        let w = SizeCappedFile::open(&path, 1024).unwrap();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = w.inner.lock().unwrap();
            panic!("simulate a poisoned log lock");
        }));
        assert!(panicked.is_err());
        let mut w = w;
        w.write_all(b"after-poison\n").unwrap();
        w.flush().unwrap();
        assert_eq!(read(&path), "after-poison\n");
    }
}
