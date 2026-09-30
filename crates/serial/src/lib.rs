//! `fs_serial` — 串口（UART）传输层（M7.4）。
//!
//! # 目标形态
//!
//! 用户裁定（2026-08-28）：面向**只有串口的裸板**，不是 SSH 可达的开发板。所以这条路上
//! 没有认证、没有加密、没有会话协议——打开一个设备，双向搬字节，就是全部。
//!
//! # 与 SSH 并列，而不是「SSH 的一个开关」
//!
//! 串口是与 SSH 并列的**传输**：它有自己的参数（波特率/数据位/校验/停止位/流控）、自己的
//! 失败模式（口被占着、拔线）、自己的重连语义（不设次数上限，见 [`reconnect`]）。
//!
//! 但**终端那一侧完全复用**：[`link::open`] 返回的 `SerialStream` 是 `AsyncRead + AsyncWrite`，
//! 正好是 `fs_terminal::SessionPipe::spawn` 要的形状。于是网格、环形缓冲、合批背压、ack、
//! 录制、会话日志、编码解码一件都不用重写——这也是出口标准「串口会话与 SSH 会话在标签/
//! 状态栏/历史等公共设施上行为一致」能成立的原因：它们本来就是同一条管道。
//!
//! # 三平台
//!
//! 端口名与枚举方式各不相同（`COM3` / `/dev/ttyUSB0` / `/dev/tty.usbserial-…`），但都由
//! `serialport` 抹平。Linux 上刻意**不开** libudev 特性：开着的话构建要系统装 libudev-dev，
//! 而枚举退化成扫 sysfs 之后串口名与参数照常可用，只是 USB 描述信息可能少几项。

pub mod link;
pub mod params;
pub mod ports;
pub mod reconnect;
pub mod session;

pub use params::{DataBits, FlowControl, Parity, SerialParams, StopBits};
pub use ports::{PortInfo, PortKind};
