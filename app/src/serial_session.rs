//! 串口会话在 app 层的落点（M7.4）。
//!
//! # 为什么另起一张注册表而不是塞进 `LiveSession`
//!
//! `LiveSession` 的每一个字段都是 SSH 的：russh 的写半部、russh 的连接句柄、跳板端点、
//! 主机密钥重连状态。串口一个都没有，反过来串口的波特率/端口名它也放不下。硬塞的结果是
//! 一堆 `Option` 与「这条路径对串口不成立」的注释，而每个 `unwrap` 都是一次未来的崩溃。
//!
//! # 但**终端那一侧完全共用**
//!
//! 两者都持有一个 `SessionPipe`，都经 `term:data:{session_id}` 出字节，都收 `term_input` /
//! `term_ack`。于是 `term_*` 那几条命令只需在 SSH 注册表落空时回落到这张表——前端一行都不用改，
//! 标签、状态栏、滚动缓冲、录制、编码切换、会话日志全部原样可用。
//! 这就是出口标准「串口会话与 SSH 会话在公共设施上行为一致」的实现方式：不是「照着做一遍」，
//! 而是**本来就是同一条管道**。

use fs_serial::session::SerialLink;
use fs_terminal::pipe::SessionPipe;
use std::sync::Arc;

pub struct SerialSession {
    pub pipe: Arc<SessionPipe>,
    pub link: SerialLink,
    /// 端口名（`COM3` / `/dev/ttyUSB0`）。状态栏的「主机」段显示它。
    pub port: String,
}

/// 串口档案参数（存进 SQLite 的那份）→ 传输层参数。
///
/// 两边的字符串取值必须逐字相同，否则**用户存的配置会读成默认值**：他配的是 9600 7E1，
/// 连上去却是 115200 8N1，现象是「连上了全是乱码」而档案里明明写着对的值。
/// 认不出的取值一律回落到该栏的默认并**留一条日志**——不是静默，也不是拒连：
/// 一条坏掉的枚举值不该让用户连不上他的板子。
pub fn to_params(s: &fs_connmgr::SerialSettings) -> fs_serial::SerialParams {
    use fs_serial::{DataBits, FlowControl, Parity, StopBits};
    let warn = |field: &str, got: &str| {
        tracing::warn!(field, got, "串口档案里有认不出的取值，按默认处理");
    };
    let data_bits = match s.data_bits.as_str() {
        "five" => DataBits::Five,
        "six" => DataBits::Six,
        "seven" => DataBits::Seven,
        "eight" => DataBits::Eight,
        other => {
            warn("data_bits", other);
            DataBits::Eight
        }
    };
    let parity = match s.parity.as_str() {
        "none" => Parity::None,
        "odd" => Parity::Odd,
        "even" => Parity::Even,
        other => {
            warn("parity", other);
            Parity::None
        }
    };
    let stop_bits = match s.stop_bits.as_str() {
        "one" => StopBits::One,
        "two" => StopBits::Two,
        other => {
            warn("stop_bits", other);
            StopBits::One
        }
    };
    let flow = match s.flow.as_str() {
        "none" => FlowControl::None,
        "software" => FlowControl::Software,
        "hardware" => FlowControl::Hardware,
        other => {
            warn("flow", other);
            FlowControl::None
        }
    };
    fs_serial::SerialParams {
        port: s.port.clone(),
        baud: s.baud,
        data_bits,
        parity,
        stop_bits,
        flow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 档案格式（`fs_connmgr::SerialSettings`）与传输层参数（`fs_serial::SerialParams`）
    /// 的**取值必须逐字对齐**。两处定义分居两个 crate（理由见 `SerialSettings` 的文档），
    /// 而分叉的表现是用户配的 9600 7E1 被读成 115200 8N1——「连上了全是乱码」。
    ///
    /// 这条测试遍历 `fs_serial` 那一侧的每个变体，用它序列化出来的字符串反向喂给
    /// `to_params`，要求原样回来。少一个变体 = 那个变体的档案值会被静默吃掉。
    #[test]
    fn wire_shape_matches_fs_serial() {
        use fs_serial::{DataBits, FlowControl, Parity, StopBits};
        fn label<T: serde::Serialize>(v: &T) -> String {
            serde_json::to_string(v)
                .unwrap()
                .trim_matches('"')
                .to_string()
        }
        for d in [
            DataBits::Five,
            DataBits::Six,
            DataBits::Seven,
            DataBits::Eight,
        ] {
            let s = fs_connmgr::SerialSettings {
                port: "COM3".into(),
                data_bits: label(&d),
                ..Default::default()
            };
            assert_eq!(to_params(&s).data_bits, d, "data_bits {d:?}");
        }
        for p in [Parity::None, Parity::Odd, Parity::Even] {
            let s = fs_connmgr::SerialSettings {
                port: "COM3".into(),
                parity: label(&p),
                ..Default::default()
            };
            assert_eq!(to_params(&s).parity, p, "parity {p:?}");
        }
        for b in [StopBits::One, StopBits::Two] {
            let s = fs_connmgr::SerialSettings {
                port: "COM3".into(),
                stop_bits: label(&b),
                ..Default::default()
            };
            assert_eq!(to_params(&s).stop_bits, b, "stop_bits {b:?}");
        }
        for f in [
            FlowControl::None,
            FlowControl::Software,
            FlowControl::Hardware,
        ] {
            let s = fs_connmgr::SerialSettings {
                port: "COM3".into(),
                flow: label(&f),
                ..Default::default()
            };
            assert_eq!(to_params(&s).flow, f, "flow {f:?}");
        }
    }

    /// 档案默认值（存量行的 `'{}'`）读出来就该是嵌入式世界的默认：115200 8N1 无流控。
    #[test]
    fn empty_settings_are_the_embedded_defaults() {
        let s: fs_connmgr::SerialSettings = serde_json::from_str("{}").unwrap();
        let p = to_params(&s);
        assert_eq!(p.baud, 115_200);
        assert_eq!(p.summary(), "115200 8N1");
    }

    /// 坏枚举值回落默认并留日志——不是静默改写，也不是拒连。
    #[test]
    fn unknown_values_fall_back_instead_of_failing_the_connection() {
        let s = fs_connmgr::SerialSettings {
            port: "COM3".into(),
            baud: 9600,
            data_bits: "nine".into(),
            parity: "space".into(),
            stop_bits: "one-and-a-half".into(),
            flow: "dtr".into(),
        };
        let p = to_params(&s);
        assert_eq!(p.baud, 9600, "认得的那些栏不受影响");
        assert_eq!(p.summary(), "9600 8N1");
    }
}
