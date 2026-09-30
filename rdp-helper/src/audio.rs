//! 音频重定向（RDPSND）后端：把远端的声音经管道送给主程序播放。
//!
//! # 为什么 helper 不自己播
//!
//! 与剪贴板同一条理由：helper 是沙箱进程，不碰 GUI 会话、不碰设备。
//! 它把 PCM 样本原样上行，主程序开输出设备。多一跳管道换来的是
//! 「解析远端不可信字节的那一侧连声卡都碰不到」。
//!
//! # 只通告 PCM，不做解码
//!
//! RDPSND 允许协商 ADPCM/MP3/AAC 等编码。做那些要在 helper 里塞一个音频
//! 解码器——而 helper 正是要把解析面关小的那一侧，往里加一整个解码器
//! 与整个设计相反。只通告 PCM 让**服务器**转码（Windows 一定支持，
//! 这是 RDP 客户端的标准做法）。
//!
//! 代价如实记下：PCM 的带宽比压缩格式大（44.1kHz 立体声 16 位 ≈ 1.4 Mbps）。
//! 局域网无所谓，跨广域网的高延迟链路上音频会先于画面卡——那时用户该做的
//! 是在远端关掉音频重定向，而不是让我们在这里塞一个解码器。
//!
//! # 迟到的音频要丢，不像画面那样合并
//!
//! 画面可以「后写覆盖前写」（见 fb.rs），音频不行：把两段声音叠加成一段是
//! 噪音。而积压的音频也不能全放——人耳听得出 200ms 的不同步。故时间戳
//! 原样上行，**由主程序决定丢哪一段**（它才知道播放缓冲的深度）。

use fs_rdpproto::FromHelper;
use ironrdp_rdpsnd::client::RdpsndClientHandler;
use ironrdp_rdpsnd::pdu::{AudioFormat, PitchPdu, VolumePdu, WaveFormat};
use std::borrow::Cow;
use tokio::sync::mpsc;

use crate::engine::ToMain;

/// 我们通告的唯一格式：44.1kHz 立体声 16 位小端 PCM。
///
/// 单一格式而不是列一串让服务器挑：格式协商每多一项就多一条要验证的路径，
/// 而 44.1/16/2 是所有 Windows 版本都能转出来的公分母。
fn pcm_44100_stereo_16() -> AudioFormat {
    AudioFormat {
        format: WaveFormat::PCM,
        n_channels: 2,
        n_samples_per_sec: 44_100,
        // 44100 × 2 声道 × 2 字节
        n_avg_bytes_per_sec: 44_100 * 2 * 2,
        // 一个采样帧的字节数：2 声道 × 2 字节
        n_block_align: 4,
        bits_per_sample: 16,
        data: None,
    }
}

#[derive(Debug)]
pub struct PipeAudio {
    to_main: mpsc::UnboundedSender<ToMain>,
    formats: Vec<AudioFormat>,
    /// 已通告过格式：`wave` 每次都带 format_no，但主程序只需在**格式变化时**
    /// 重开设备。不记这一位就会每来一段音频发一条 AudioFormat，
    /// 主程序那侧要么反复重开设备（爆音），要么自己去记——而它记的
    /// 正是这里本就有的信息。
    announced: Option<usize>,
}

impl PipeAudio {
    pub fn new(to_main: mpsc::UnboundedSender<ToMain>) -> Self {
        Self {
            to_main,
            formats: vec![pcm_44100_stereo_16()],
            announced: None,
        }
    }

    fn up(&self, header: FromHelper, body: Vec<u8>) {
        let _ = self.to_main.send(ToMain { header, body });
    }
}

impl RdpsndClientHandler for PipeAudio {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn wave(&mut self, format_no: usize, ts: u32, data: Cow<'_, [u8]>) {
        // 服务器理论上只会用我们通告过的格式。真给了别的（对端实现宽松）
        // 就丢掉并留痕——按错误的格式播放出来的是刺耳的白噪音，
        // 那比没有声音糟糕得多。
        let Some(fmt) = self.formats.get(format_no) else {
            eprintln!("fs-rdp-helper: 音频格式号 {format_no} 未通告过，丢弃该段");
            return;
        };
        if self.announced != Some(format_no) {
            self.announced = Some(format_no);
            self.up(
                FromHelper::AudioFormat {
                    sample_rate: fmt.n_samples_per_sec,
                    channels: fmt.n_channels,
                    bits_per_sample: fmt.bits_per_sample,
                },
                Vec::new(),
            );
        }
        self.up(
            FromHelper::AudioData { timestamp_ms: ts },
            data.into_owned(),
        );
    }

    fn set_volume(&mut self, _volume: VolumePdu) {
        // 音量由**本机**控制（系统混音器里那一条）。远端设的音量不转发：
        // 那会让用户在本机调好的音量被远端悄悄改掉，而他找不到是谁改的。
    }

    fn set_pitch(&mut self, _pitch: PitchPdu) {
        // 变调：Windows 自己都没实现（MS-RDPEA 说明该 PDU 可忽略）。
    }

    fn close(&mut self) {
        self.announced = None;
        self.up(FromHelper::AudioClose, Vec::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig() -> (PipeAudio, mpsc::UnboundedReceiver<ToMain>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (PipeAudio::new(tx), rx)
    }

    /// 通告的格式是 PCM——不是压缩格式（helper 里不放解码器，见模块头）。
    #[test]
    fn only_pcm_is_advertised() {
        let (a, _rx) = rig();
        let fmts = a.get_formats();
        assert_eq!(fmts.len(), 1, "只通告一种格式（协商路径越少越好）");
        assert_eq!(fmts[0].format, WaveFormat::PCM);
        assert_eq!(fmts[0].bits_per_sample, 16);
        assert_eq!(fmts[0].n_channels, 2);
        assert_eq!(fmts[0].n_samples_per_sec, 44_100);
        // n_block_align 与 n_avg_bytes_per_sec 必须与上面三项自洽，
        // 否则 Windows 侧会按错误的步长切样本（听感是变速+爆音）
        assert_eq!(fmts[0].n_block_align, 4);
        assert_eq!(fmts[0].n_avg_bytes_per_sec, 44_100 * 4);
    }

    /// **格式只在变化时通告一次**：每段音频都发一条会让主程序反复重开设备。
    #[test]
    fn format_is_announced_once_not_per_chunk() {
        let (mut a, mut rx) = rig();
        a.wave(0, 100, Cow::Borrowed(&[1, 2, 3, 4]));
        a.wave(0, 200, Cow::Borrowed(&[5, 6, 7, 8]));
        let mut formats = 0;
        let mut datas = 0;
        while let Ok(m) = rx.try_recv() {
            match m.header {
                FromHelper::AudioFormat { .. } => formats += 1,
                FromHelper::AudioData { .. } => datas += 1,
                _ => {}
            }
        }
        assert_eq!(formats, 1, "两段音频只该有一条格式通告");
        assert_eq!(datas, 2);
    }

    /// PCM 样本**逐字节原样**上行（体不经任何转换）。
    #[test]
    fn samples_pass_through_byte_for_byte() {
        let (mut a, mut rx) = rig();
        // 含 0x00 与 0xFF：这两个值最容易被「当字符串处理」的实现损坏
        let pcm = vec![0x00, 0xFF, 0x80, 0x7F];
        a.wave(0, 0, Cow::Borrowed(&pcm));
        let mut got = None;
        while let Ok(m) = rx.try_recv() {
            if matches!(m.header, FromHelper::AudioData { .. }) {
                got = Some(m.body);
            }
        }
        assert_eq!(got.as_deref(), Some(&pcm[..]));
    }

    /// 时间戳原样带上——主程序据它判断积压该丢哪段（迟到的音频没有意义）。
    #[test]
    fn timestamp_is_carried_through() {
        let (mut a, mut rx) = rig();
        a.wave(0, 12_345, Cow::Borrowed(&[0, 0]));
        let mut ts = None;
        while let Ok(m) = rx.try_recv() {
            if let FromHelper::AudioData { timestamp_ms } = m.header {
                ts = Some(timestamp_ms);
            }
        }
        assert_eq!(ts, Some(12_345));
    }

    /// **未通告过的格式号要丢弃**：按错误格式播出来是刺耳白噪音，
    /// 比没有声音糟糕得多。
    #[test]
    fn an_unadvertised_format_is_dropped_not_played() {
        let (mut a, mut rx) = rig();
        a.wave(99, 0, Cow::Borrowed(&[1, 2]));
        assert!(rx.try_recv().is_err(), "不该发出任何音频消息");
    }

    /// close 之后再来音频，格式要重新通告（设备已被主程序关掉）。
    #[test]
    fn format_is_re_announced_after_close() {
        let (mut a, mut rx) = rig();
        a.wave(0, 0, Cow::Borrowed(&[0, 0]));
        a.close();
        a.wave(0, 10, Cow::Borrowed(&[0, 0]));
        let mut formats = 0;
        while let Ok(m) = rx.try_recv() {
            if matches!(m.header, FromHelper::AudioFormat { .. }) {
                formats += 1;
            }
        }
        assert_eq!(formats, 2, "close 之后必须重新通告格式");
    }
}
