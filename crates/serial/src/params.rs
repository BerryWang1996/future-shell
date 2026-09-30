//! 串口参数（M7.4）：波特率、数据位、校验、停止位、流控。
//!
//! # 为什么这些值要自己定义一遍，而不是直接用 serialport 的枚举
//!
//! 这些参数要**跨三条边界**：存进 SQLite 里的 profile JSON、经 Tauri IPC 到前端、
//! 再回到 `serialport`。中间两条边界需要 `serde`，而上游的枚举没有稳定的序列化契约——
//! 直接用它等于把「档案文件的格式」绑在一个我们不控制的库的内部表示上，那个库改一次
//! 命名，用户存了半年的档案就读不出来了。
//!
//! 这一层还承担**校验**：波特率是 u32，前端能塞进来 0 或 40 亿。0 在 Windows 上是
//! `SetCommState` 直接失败，在 Linux 上是「挂断」（B0 = 放弃 DTR），两种都不是用户要的。

use serde::{Deserialize, Serialize};

/// 数据位。5/6 位是历史遗物（电传打字机），但工控设备上真的还有在用的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DataBits {
    Five,
    Six,
    Seven,
    #[default]
    Eight,
}

/// 校验位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
}

/// 停止位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StopBits {
    #[default]
    One,
    Two,
}

/// 流控。
///
/// 默认 `None` 而不是硬件流控：裸板常常只接了 TX/RX/GND 三根线，选了硬件流控就是
/// **一个字节都发不出去**（等 CTS 等到天荒地老），而现象是「连上了但什么都没有」——
/// 用户几乎不可能猜到是流控。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FlowControl {
    #[default]
    None,
    /// XON/XOFF。终端里慎用：Ctrl+S/Ctrl+Q 会被当成流控字符吃掉。
    Software,
    /// RTS/CTS。
    Hardware,
}

/// 界面上给的常用波特率。**不是白名单**——自定义值照样接受（工控设备上 250000、
/// 500000 这类非标值很常见），这只是一份省得用户打字的清单。
pub const COMMON_BAUDS: &[u32] = &[
    9600, 19200, 38400, 57600, 115200, 230400, 460800, 500000, 921600, 1_000_000, 1_500_000,
    2_000_000,
];

/// 波特率下限。低于 50 的档在三个平台上都不是「很慢」而是「非法/挂断」。
pub const BAUD_MIN: u32 = 50;
/// 波特率上限。4 Mbaud 已经超过绝大多数 USB-TTL 芯片的能力；再高的值多半是填错了
/// （多打一个 0），而它在 Linux 上会被 `cfsetspeed` 直接拒绝。
pub const BAUD_MAX: u32 = 4_000_000;

/// 打开一个串口所需的全部参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialParams {
    /// 端口名：Windows 是 `COM3`，Linux 是 `/dev/ttyUSB0`，macOS 是 `/dev/tty.usbserial-1420`。
    pub port: String,
    pub baud: u32,
    #[serde(default)]
    pub data_bits: DataBits,
    #[serde(default)]
    pub parity: Parity,
    #[serde(default)]
    pub stop_bits: StopBits,
    #[serde(default)]
    pub flow: FlowControl,
}

impl Default for SerialParams {
    fn default() -> Self {
        Self {
            port: String::new(),
            // 115200 8N1：嵌入式默认值里最常见的一组（U-Boot / Linux console / ESP32 全是它）。
            baud: 115_200,
            data_bits: DataBits::default(),
            parity: Parity::default(),
            stop_bits: StopBits::default(),
            flow: FlowControl::default(),
        }
    }
}

impl SerialParams {
    /// 校验。错误文案直接给用户看，所以要说清**为什么不行**而不是「参数非法」。
    pub fn validate(&self) -> Result<(), String> {
        if self.port.trim().is_empty() {
            return Err("没有选择串口".into());
        }
        // 端口名里的控制字符/换行：不是用户手打出来的，是粘贴或改坏了配置。
        // 放过去的话，Windows 上会变成一个奇怪的设备路径，Linux 上会变成一个奇怪的文件名。
        if self.port.chars().any(|c| c.is_control()) {
            return Err("串口名里有控制字符".into());
        }
        if self.baud < BAUD_MIN || self.baud > BAUD_MAX {
            return Err(format!(
                "波特率 {} 超出范围（{BAUD_MIN}–{BAUD_MAX}）",
                self.baud
            ));
        }
        Ok(())
    }

    /// 人读的参数摘要，形如 `115200 8N1`。状态栏与标签标题用。
    ///
    /// 这个缩写是串口世界的通用写法，比「波特率 115200，数据位 8，无校验，1 停止位」
    /// 短得多也更容易一眼扫过——而状态栏只有一行。
    pub fn summary(&self) -> String {
        let d = match self.data_bits {
            DataBits::Five => 5,
            DataBits::Six => 6,
            DataBits::Seven => 7,
            DataBits::Eight => 8,
        };
        let p = match self.parity {
            Parity::None => 'N',
            Parity::Odd => 'O',
            Parity::Even => 'E',
        };
        let s = match self.stop_bits {
            StopBits::One => 1,
            StopBits::Two => 2,
        };
        let mut out = format!("{} {}{}{}", self.baud, d, p, s);
        match self.flow {
            FlowControl::None => {}
            FlowControl::Software => out.push_str(" XON/XOFF"),
            FlowControl::Hardware => out.push_str(" RTS/CTS"),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_the_embedded_world_default() {
        let p = SerialParams::default();
        assert_eq!(p.baud, 115_200);
        assert_eq!(p.data_bits, DataBits::Eight);
        assert_eq!(p.parity, Parity::None);
        assert_eq!(p.stop_bits, StopBits::One);
        // 默认无流控：裸板常常只接三根线，默认硬件流控 = 一个字节都发不出去
        assert_eq!(p.flow, FlowControl::None);
    }

    #[test]
    fn summary_reads_like_a_serial_console() {
        let mut p = SerialParams {
            port: "COM3".into(),
            ..Default::default()
        };
        assert_eq!(p.summary(), "115200 8N1");
        p.parity = Parity::Even;
        p.data_bits = DataBits::Seven;
        p.stop_bits = StopBits::Two;
        assert_eq!(p.summary(), "115200 7E2");
        p.flow = FlowControl::Hardware;
        assert!(p.summary().ends_with("RTS/CTS"));
    }

    #[test]
    fn empty_port_is_rejected_with_a_human_reason() {
        let p = SerialParams::default();
        assert!(p.validate().unwrap_err().contains("没有选择串口"));
    }

    #[test]
    fn control_chars_in_port_name_are_rejected() {
        let p = SerialParams {
            port: "COM3\n".into(),
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    /// 0 波特在 Windows 上是失败、在 Linux 上是**挂断**（B0 放弃 DTR）——
    /// 两种都不是用户要的，且第二种表现为「连上了但一片死寂」。
    #[test]
    fn baud_bounds() {
        let ok = |b: u32| {
            SerialParams {
                port: "COM1".into(),
                baud: b,
                ..Default::default()
            }
            .validate()
            .is_ok()
        };
        assert!(!ok(0));
        assert!(!ok(BAUD_MIN - 1));
        assert!(ok(BAUD_MIN));
        assert!(ok(115_200));
        assert!(ok(BAUD_MAX));
        assert!(!ok(BAUD_MAX + 1));
        // 非标波特率必须放行：工控设备上 250000 / 500000 很常见
        assert!(ok(250_000));
    }

    /// 常用清单只是省打字，不是白名单——上一条已经证明非标值可用，这里钉清单本身没跑偏。
    #[test]
    fn common_bauds_are_sane_and_sorted() {
        assert!(COMMON_BAUDS.contains(&115_200));
        assert!(COMMON_BAUDS.windows(2).all(|w| w[0] < w[1]), "清单要升序");
        assert!(COMMON_BAUDS
            .iter()
            .all(|b| (BAUD_MIN..=BAUD_MAX).contains(b)));
    }

    /// 序列化契约：档案 JSON 里存的是 snake_case 字符串。改了它 = 用户存了半年的档案读不出来。
    #[test]
    fn wire_format_is_snake_case_strings() {
        let p = SerialParams {
            port: "/dev/ttyUSB0".into(),
            baud: 9600,
            data_bits: DataBits::Seven,
            parity: Parity::Even,
            stop_bits: StopBits::Two,
            flow: FlowControl::Software,
        };
        let j = serde_json::to_string(&p).unwrap();
        assert!(j.contains(r#""data_bits":"seven""#), "{j}");
        assert!(j.contains(r#""parity":"even""#), "{j}");
        assert!(j.contains(r#""stop_bits":"two""#), "{j}");
        assert!(j.contains(r#""flow":"software""#), "{j}");
        assert_eq!(serde_json::from_str::<SerialParams>(&j).unwrap(), p);
    }

    /// 旧档案（只有 port + baud）要能读出来并吃默认值——加字段不能让存量档案变成坏行。
    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let p: SerialParams = serde_json::from_str(r#"{"port":"COM3","baud":115200}"#).unwrap();
        assert_eq!(
            p,
            SerialParams {
                port: "COM3".into(),
                ..Default::default()
            }
        );
    }
}
