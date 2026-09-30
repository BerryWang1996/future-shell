//! 串口枚举（M7.4）。
//!
//! # 为什么枚举结果要排序与去重
//!
//! 三个平台给出的顺序都不是「人看的顺序」：Windows 按注册表键序（`COM10` 排在 `COM2`
//! 前面），Linux 按 sysfs 扫描序（`ttyUSB1` 可能在 `ttyUSB0` 前面）。用户面对的是一个
//! 下拉框，顺序乱跳意味着**每次插拔之后要重新找一遍**——而串口用户经常插着三四个转换器。
//!
//! # 为什么要把 USB 信息带出来
//!
//! `COM7` 这个名字不告诉用户它是哪根线。带上厂商/产品/序列号之后，下拉里看到的是
//! 「COM7 — CP2102 USB to UART Bridge」，插着两个板子时才分得清。

use serde::{Deserialize, Serialize};

/// 端口类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortKind {
    Usb,
    Bluetooth,
    Pci,
    /// 板载 16550 之类，或平台没给类型信息。
    Native,
}

/// 一个可用串口。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortInfo {
    /// 打开它要用的名字（`COM3` / `/dev/ttyUSB0`）。
    pub name: String,
    pub kind: PortKind,
    /// 人读描述（厂商/产品/序列号拼出来的），没有则 `None`。
    pub description: Option<String>,
}

/// 把 USB 描述符拼成一句人能读的话。
///
/// 三段都可能缺（Linux 上不装 libudev 时经常只有 VID/PID）。全缺时回落成十六进制的
/// `VID:PID`——那至少还能拿去搜，比空白强。
pub fn describe_usb(
    manufacturer: Option<&str>,
    product: Option<&str>,
    serial: Option<&str>,
    vid: u16,
    pid: u16,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = manufacturer.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(m.to_string());
    }
    if let Some(p) = product.map(str::trim).filter(|s| !s.is_empty()) {
        // 厂商名常常已经在产品名里（"Silicon Labs CP2102 USB to UART Bridge"），
        // 再拼一遍就成了「Silicon Labs Silicon Labs CP2102…」。
        if !parts.iter().any(|m| p.starts_with(m.as_str())) {
            parts.push(p.to_string());
        } else {
            parts.clear();
            parts.push(p.to_string());
        }
    }
    if parts.is_empty() {
        parts.push(format!("USB {vid:04x}:{pid:04x}"));
    }
    if let Some(s) = serial.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("#{s}"));
    }
    parts.join(" ")
}

/// 端口名的**自然序**键：把名字里的数字段按数值比。
///
/// `COM10` 与 `COM2` 按字典序会排反，而用户是按编号找口的。
fn natural_key(name: &str) -> Vec<(bool, u64, String)> {
    let mut out = Vec::new();
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.peek().copied() {
        if c.is_ascii_digit() {
            let mut n: u64 = 0;
            while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                // 饱和：一个 30 位的「数字」不是编号，是坏名字；不让它 panic
                n = n.saturating_mul(10).saturating_add(d as u64);
                chars.next();
            }
            out.push((true, n, String::new()));
        } else {
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    break;
                }
                s.push(c.to_ascii_lowercase());
                chars.next();
            }
            out.push((false, 0, s));
        }
    }
    out
}

/// 排序 + 去重（同名只留一条）。**纯函数**，因此可测——真实枚举依赖插着什么硬件。
///
/// 次序：USB 在前（用户新插的那个几乎总是 USB 转串口），然后按名字自然序。
pub fn sort_ports(mut ports: Vec<PortInfo>) -> Vec<PortInfo> {
    ports.sort_by(|a, b| {
        let rank = |k: PortKind| match k {
            PortKind::Usb => 0,
            PortKind::Bluetooth => 1,
            PortKind::Pci => 2,
            PortKind::Native => 3,
        };
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then_with(|| natural_key(&a.name).cmp(&natural_key(&b.name)))
    });
    ports.dedup_by(|a, b| a.name == b.name);
    ports
}

/// 枚举当前可用的串口。
///
/// 失败**不当成致命错**：枚举拿不到只意味着列表是空的，用户仍然可以手打端口名
///（这在 Linux 上很常见——不装 libudev 时某些虚拟串口不出现在 sysfs 扫描里）。
/// 所以返回 `Err` 的场合极少，且调用方应当把它降级成「列表为空 + 一行说明」。
pub fn list_ports() -> Result<Vec<PortInfo>, String> {
    let raw = tokio_serial::available_ports().map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(raw.len());
    for p in raw {
        let (kind, description) = match &p.port_type {
            tokio_serial::SerialPortType::UsbPort(u) => (
                PortKind::Usb,
                Some(describe_usb(
                    u.manufacturer.as_deref(),
                    u.product.as_deref(),
                    u.serial_number.as_deref(),
                    u.vid,
                    u.pid,
                )),
            ),
            tokio_serial::SerialPortType::BluetoothPort => (PortKind::Bluetooth, None),
            tokio_serial::SerialPortType::PciPort => (PortKind::Pci, None),
            tokio_serial::SerialPortType::Unknown => (PortKind::Native, None),
        };
        out.push(PortInfo {
            name: p.port_name,
            kind,
            description,
        });
    }
    Ok(sort_ports(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, kind: PortKind) -> PortInfo {
        PortInfo {
            name: name.into(),
            kind,
            description: None,
        }
    }

    /// COM10 排在 COM2 后面——字典序会排反，而用户是按编号找口的。
    #[test]
    fn numbers_sort_numerically() {
        let got = sort_ports(vec![
            p("COM10", PortKind::Native),
            p("COM2", PortKind::Native),
            p("COM1", PortKind::Native),
        ]);
        assert_eq!(
            got.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["COM1", "COM2", "COM10"]
        );
    }

    #[test]
    fn unix_device_names_sort_naturally_too() {
        let got = sort_ports(vec![
            p("/dev/ttyUSB10", PortKind::Usb),
            p("/dev/ttyUSB2", PortKind::Usb),
            p("/dev/ttyACM0", PortKind::Usb),
        ]);
        assert_eq!(
            got.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["/dev/ttyACM0", "/dev/ttyUSB2", "/dev/ttyUSB10"]
        );
    }

    /// USB 排在前面：用户刚插上的那根几乎总是 USB 转串口，而板载口一年也用不到一次。
    #[test]
    fn usb_ports_come_first() {
        let got = sort_ports(vec![
            p("COM1", PortKind::Native),
            p("COM9", PortKind::Usb),
            p("COM5", PortKind::Bluetooth),
        ]);
        assert_eq!(
            got.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["COM9", "COM5", "COM1"]
        );
    }

    #[test]
    fn duplicates_are_dropped() {
        let got = sort_ports(vec![
            p("COM3", PortKind::Usb),
            p("COM3", PortKind::Usb),
            p("COM4", PortKind::Usb),
        ]);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn usb_description_reads_like_a_label() {
        assert_eq!(
            describe_usb(
                Some("Silicon Labs"),
                Some("CP2102 USB to UART Bridge"),
                None,
                0x10c4,
                0xea60
            ),
            "Silicon Labs CP2102 USB to UART Bridge"
        );
        // 产品名里已经含厂商名时不重复拼
        assert_eq!(
            describe_usb(
                Some("FTDI"),
                Some("FTDI FT232R USB UART"),
                Some("A50285BI"),
                0x0403,
                0x6001
            ),
            "FTDI FT232R USB UART #A50285BI"
        );
        // 全缺时回落成可搜索的 VID:PID，而不是空白
        assert_eq!(
            describe_usb(None, None, None, 0x1a86, 0x7523),
            "USB 1a86:7523"
        );
        // 空串等同于缺失（Linux 上常见的空描述符）
        assert_eq!(
            describe_usb(Some("  "), Some(""), None, 0x1a86, 0x7523),
            "USB 1a86:7523"
        );
    }

    /// 枚举本身依赖插着什么硬件，只能钉「不 panic、结果自洽」。
    #[test]
    fn enumeration_does_not_panic_and_is_sorted() {
        let ports = list_ports().unwrap_or_default();
        let names: Vec<_> = ports.iter().map(|p| p.name.clone()).collect();
        let mut expect = names.clone();
        expect.dedup();
        assert_eq!(names, expect, "枚举结果里有重名");
        assert_eq!(sort_ports(ports.clone()), ports, "枚举结果没有按显示序排好");
    }
}
