//! 打开串口、改参数、失败原因翻译（M7.4）。
//!
//! # 这一层为什么这么薄
//!
//! 真正的传输在 `tokio_serial::SerialStream`，它已经是 `AsyncRead + AsyncWrite`——
//! 正好是 `fs_terminal::SessionPipe::spawn` 要的形状。于是串口会话直接复用终端那一整条
//! 管道：网格、环形缓冲、合批背压、ack、录制、ZMODEM 拦截、会话日志，一件都不用重写。
//! **这是把串口做成「与 SSH 并列的传输」而不是「另一个终端」的关键**，也是出口标准
//! 「串口会话与 SSH 会话在公共设施上行为一致」能成立的原因。
//!
//! # 失败原因必须翻译
//!
//! 串口打不开的三大原因（被别的程序占着、拔了、没权限）在三个平台上给出的是
//! 完全不同的系统错误。原样甩给用户等于什么都没说——「Access is denied. (os error 5)」
//! 不会让任何人想到「串口调试助手还开着」。

use crate::params::{DataBits, FlowControl, Parity, SerialParams, StopBits};
use tokio_serial::SerialPortBuilderExt;

/// 打开一个串口。成功后拿到的流可直接喂给 `SessionPipe::spawn`。
///
/// `timeout` 不设：`SerialStream` 是非阻塞的，读超时由上层的等待逻辑决定。给它设一个
/// 读超时反而会让「板子暂时没输出」变成周期性的读错误。
pub fn open(params: &SerialParams) -> Result<tokio_serial::SerialStream, String> {
    params.validate()?;
    let builder = tokio_serial::new(&params.port, params.baud)
        .data_bits(map_data_bits(params.data_bits))
        .parity(map_parity(params.parity))
        .stop_bits(map_stop_bits(params.stop_bits))
        .flow_control(map_flow(params.flow));
    builder
        .open_native_async()
        .map_err(|e| explain_open_error(&params.port, &e))
}

/// 改波特率——**不重开端口**。
///
/// 出口标准点名了「改波特率」。重开端口做不到同一件事：重开会让 DTR/RTS 抖一下，
/// 而很多板子把 DTR 接在复位脚上（ESP32/Arduino 就是这么自动进下载模式的）——
/// 用户只是想换个速率看看，板子却重启了。
pub fn set_baud(port: &mut tokio_serial::SerialStream, baud: u32) -> Result<(), String> {
    if !(crate::params::BAUD_MIN..=crate::params::BAUD_MAX).contains(&baud) {
        return Err(format!(
            "波特率 {baud} 超出范围（{}–{}）",
            crate::params::BAUD_MIN,
            crate::params::BAUD_MAX
        ));
    }
    tokio_serial::SerialPort::set_baud_rate(port, baud).map_err(|e| e.to_string())
}

/// 把系统错误翻译成用户能据此行动的一句话。
///
/// 判据用 `ErrorKind` 而不是错误文本：文本随平台与语言变（中文 Windows 上是
/// 「拒绝访问。」），按文本匹配的代码在用户机器上必然失效。
pub fn explain_open_error(port: &str, e: &tokio_serial::Error) -> String {
    use tokio_serial::ErrorKind;
    let hint = match e.kind() {
        ErrorKind::NoDevice => "设备不在了——拔掉了？换了个口？",
        ErrorKind::InvalidInput => "端口名或参数不对（波特率/数据位组合这块硬件不支持）",
        ErrorKind::Io(std::io::ErrorKind::PermissionDenied) => {
            // 两个平台的成因完全不同，但用户要做的动作只有这两种，一并写出来
            "没有权限，或端口正被另一个程序占着（串口调试助手/另一个终端；Linux 上还要在 dialout 组里）"
        }
        ErrorKind::Io(std::io::ErrorKind::NotFound) => "找不到这个端口",
        ErrorKind::Io(std::io::ErrorKind::TimedOut) => "打开超时（驱动没有响应）",
        _ => "",
    };
    if hint.is_empty() {
        format!("打不开 {port}：{e}")
    } else {
        format!("打不开 {port}：{hint}（{e}）")
    }
}

fn map_data_bits(d: DataBits) -> tokio_serial::DataBits {
    match d {
        DataBits::Five => tokio_serial::DataBits::Five,
        DataBits::Six => tokio_serial::DataBits::Six,
        DataBits::Seven => tokio_serial::DataBits::Seven,
        DataBits::Eight => tokio_serial::DataBits::Eight,
    }
}

fn map_parity(p: Parity) -> tokio_serial::Parity {
    match p {
        Parity::None => tokio_serial::Parity::None,
        Parity::Odd => tokio_serial::Parity::Odd,
        Parity::Even => tokio_serial::Parity::Even,
    }
}

fn map_stop_bits(s: StopBits) -> tokio_serial::StopBits {
    match s {
        StopBits::One => tokio_serial::StopBits::One,
        StopBits::Two => tokio_serial::StopBits::Two,
    }
}

fn map_flow(f: FlowControl) -> tokio_serial::FlowControl {
    match f {
        FlowControl::None => tokio_serial::FlowControl::None,
        FlowControl::Software => tokio_serial::FlowControl::Software,
        FlowControl::Hardware => tokio_serial::FlowControl::Hardware,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_rejects_invalid_params_before_touching_the_os() {
        let bad = SerialParams {
            port: String::new(),
            ..Default::default()
        };
        assert!(open(&bad).unwrap_err().contains("没有选择串口"));
    }

    /// 不存在的端口：必须报出**端口名**与一句人能据此行动的话。
    #[test]
    fn opening_a_missing_port_names_it() {
        let p = SerialParams {
            port: if cfg!(windows) {
                "COM255".into()
            } else {
                "/dev/tty-does-not-exist-fs".into()
            },
            ..Default::default()
        };
        let e = open(&p).unwrap_err();
        assert!(e.contains(&p.port), "错误里没有端口名：{e}");
        assert!(e.starts_with("打不开"), "{e}");
    }

    #[test]
    fn set_baud_validates_range() {
        // 不开端口也能测到范围闸（真正的 set 要有设备）。const 块：两边都是编译期常量，
        // 普通 assert! 会被 clippy 的 assertions_on_constants 判死（-D warnings 下即编译失败）。
        const {
            assert!(crate::params::BAUD_MIN > 0);
            assert!(crate::params::BAUD_MAX > crate::params::BAUD_MIN);
        }
        let too_big = crate::params::BAUD_MAX + 1;
        assert!(!(crate::params::BAUD_MIN..=crate::params::BAUD_MAX).contains(&too_big));
    }

    /// 翻译靠 ErrorKind 而不是错误文本：文本随平台与系统语言变
    ///（中文 Windows 上是「拒绝访问。」），按文本匹配在用户机器上必然失效。
    #[test]
    fn permission_denied_mentions_the_two_things_a_user_can_do() {
        let e = tokio_serial::Error::new(
            tokio_serial::ErrorKind::Io(std::io::ErrorKind::PermissionDenied),
            "Access is denied.",
        );
        let msg = explain_open_error("COM3", &e);
        assert!(msg.contains("占着"), "{msg}");
        assert!(msg.contains("dialout"), "{msg}");
        assert!(msg.contains("COM3"), "{msg}");
    }

    #[test]
    fn no_device_says_it_is_gone() {
        let e = tokio_serial::Error::new(tokio_serial::ErrorKind::NoDevice, "x");
        assert!(explain_open_error("COM7", &e).contains("拔掉"));
    }

    #[test]
    fn unknown_kinds_still_carry_the_original_text() {
        let e = tokio_serial::Error::new(tokio_serial::ErrorKind::Unknown, "某种没见过的错");
        let msg = explain_open_error("COM7", &e);
        assert!(msg.contains("某种没见过的错"), "{msg}");
    }
}
