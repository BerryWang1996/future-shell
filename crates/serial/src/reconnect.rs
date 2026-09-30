//! 串口断开与重连的节奏（M7.4 出口标准的「断开重连」那一档）。
//!
//! # 串口的「断开」与 SSH 的不是一回事
//!
//! SSH 断线是对端或网络出了事，重连要重新握手、重新认证，代价大、成功率低，所以有次数上限。
//! 串口断开几乎总是**同一件事**：USB 转换器被拔了（或板子掉电、驱动重枚举）。它的特点是
//! ① 重连极便宜（打开一个设备文件），② 恢复时刻完全不可预测（用户什么时候插回来），
//! ③ 用户预期是「插回去就该自己好」——像插 U 盘一样。
//!
//! 所以这里**不设次数上限**，而是退避到一个固定的巡检周期上一直等。给串口设「重试 5 次
//! 后放弃」的表现是：用户拔下来接根线、两分钟后插回去，程序早就不试了，而界面上只有一行
//! 两分钟前的旧提示。
//!
//! # 为什么退避而不是固定间隔
//!
//! 刚断开的一两秒最可能是虚接/重枚举，值得密集试几次；之后就该慢下来——每 100 ms 去
//! `open()` 一个不存在的设备在 Linux 上是一次系统调用加一次日志，插着不动的话一天就是
//! 几十万次。

use std::time::Duration;

/// 巡检周期上限：退避涨到这里就不再涨。
///
/// 2 秒是「用户插回去之后最多等 2 秒就自己好了」——比这更长会让人以为程序没在管；
/// 更短则在长时间拔线时白烧 CPU。
pub const MAX_INTERVAL: Duration = Duration::from_secs(2);

/// 第 `attempt` 次重连尝试（从 1 起）之前该等多久。
///
/// 250 ms 起步、每次翻倍、封顶 [`MAX_INTERVAL`]。attempt=0 视作 1（调用方从 1 起数，
/// 传 0 是笔误而不该是「不等」——那会变成忙等）。
pub fn backoff(attempt: u32) -> Duration {
    let n = attempt.clamp(1, 16); // 16 次就早已封顶，再大只是防溢出
    let ms = 250u64.saturating_mul(1u64 << (n - 1));
    Duration::from_millis(ms).min(MAX_INTERVAL)
}

/// 断开的成因。决定**要不要自动重连**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disconnect {
    /// 设备不见了（拔线/掉电/驱动重枚举）。这是串口最常见的断开，自动重连。
    DeviceGone,
    /// 读写出错但设备还在（线路噪声、驱动瞬时错误）。也自动重连。
    IoError,
    /// 用户主动关闭。**不重连**——重连会让「关掉的标签自己活过来」。
    UserClosed,
}

impl Disconnect {
    /// 该不该自动重连。
    pub fn should_reconnect(self) -> bool {
        !matches!(self, Disconnect::UserClosed)
    }

    /// 给用户看的一句话。
    pub fn message(self, port: &str) -> String {
        match self {
            Disconnect::DeviceGone => {
                format!("{port} 不见了（拔线/掉电）。插回去就会自动重连，标签留着。")
            }
            Disconnect::IoError => format!("{port} 读写出错，正在重连…"),
            Disconnect::UserClosed => format!("已关闭 {port}"),
        }
    }
}

/// 由 IO 错误判定断开成因。
pub fn classify_io(e: &std::io::Error) -> Disconnect {
    match e.kind() {
        // NotFound/BrokenPipe/PermissionDenied 都是「设备没了」的三平台变体：
        // Windows 拔 USB 之后读到的是 BrokenPipe/操作系统错误 22，Linux 是 ENODEV/EIO，
        // macOS 常给 ENXIO。共同点是**再读也不会好**，要等设备回来。
        std::io::ErrorKind::NotFound
        | std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::PermissionDenied
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::ConnectionReset => Disconnect::DeviceGone,
        _ => Disconnect::IoError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff(1), Duration::from_millis(250));
        assert_eq!(backoff(2), Duration::from_millis(500));
        assert_eq!(backoff(3), Duration::from_millis(1000));
        assert_eq!(backoff(4), MAX_INTERVAL);
        assert_eq!(backoff(50), MAX_INTERVAL, "封顶之后不再涨");
    }

    /// attempt=0 是调用方笔误。返回 0 会让重连循环变成忙等（每秒几十万次 open）。
    #[test]
    fn attempt_zero_is_not_a_busy_loop() {
        assert_eq!(backoff(0), backoff(1));
        assert!(backoff(0) > Duration::ZERO);
    }

    /// 串口重连**不设次数上限**：用户拔下来接根线、两分钟后插回去，
    /// 「重试 5 次后放弃」的表现是程序早不试了而界面上只有一行旧提示。
    #[test]
    fn there_is_no_attempt_ceiling_in_this_module() {
        // 上限若存在，必然表现为某个 attempt 之后 backoff 返回 None 或极大值
        for n in [1u32, 5, 100, 10_000, u32::MAX] {
            assert!(backoff(n) <= MAX_INTERVAL);
            assert!(backoff(n) > Duration::ZERO);
        }
    }

    #[test]
    fn user_close_never_reconnects() {
        assert!(!Disconnect::UserClosed.should_reconnect());
        assert!(Disconnect::DeviceGone.should_reconnect());
        assert!(Disconnect::IoError.should_reconnect());
    }

    /// 拔线的提示必须说清两件事：它还会自己好、标签不会消失。
    #[test]
    fn device_gone_message_tells_the_user_what_will_happen() {
        let m = Disconnect::DeviceGone.message("COM3");
        assert!(m.contains("COM3"));
        assert!(m.contains("自动重连"));
        assert!(m.contains("标签留着"));
    }

    #[test]
    fn io_kinds_map_to_device_gone_on_all_three_platforms() {
        use std::io::{Error, ErrorKind};
        for k in [
            ErrorKind::NotFound,
            ErrorKind::BrokenPipe,
            ErrorKind::PermissionDenied,
            ErrorKind::ConnectionAborted,
            ErrorKind::ConnectionReset,
        ] {
            assert_eq!(
                classify_io(&Error::new(k, "x")),
                Disconnect::DeviceGone,
                "{k:?}"
            );
        }
        assert_eq!(
            classify_io(&Error::new(ErrorKind::InvalidData, "x")),
            Disconnect::IoError
        );
    }
}
