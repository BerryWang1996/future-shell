//! 帧缓冲的脏矩形提取与合并（纯函数，离线可测）。
//!
//! # 为什么 RDP 的背压策略是「合并」而不是「排队」
//!
//! 终端字节**绝不能丢**（丢一个字节屏幕就全错了），所以终端管道是
//! never-drop + 确认背压。像素相反：**后写覆盖前写**——同一个位置的第 N 次更新
//! 到达时，第 N-1 次的内容已经没有意义。积压时把待发矩形**合并成包围盒**并
//! 丢弃中间态是正确的；照抄终端那套 never-drop 会让慢客户端把内存吃光
//! （一次全屏重绘 8 MB，几百帧积压就是 GB 级）。
//!
//! 代价是包围盒可能大于两个矩形的和（重画了没变的像素）。这是**用重画换内存**：
//! 前端把多出来的像素盖回同样的值，视觉无差别；而内存爆掉是会话死亡。
//!
//! 合并的上限：[`MAX_PENDING_RECTS`]。到达上限时不再合并、也不丢——直接发一个
//! 全屏矩形（全屏 = 最坏包围盒，且「整块重画」的语义最简单，前端不需要任何
//! 增量推理）。这个分支正常不该走到；走到说明前端慢到反常，全屏是最诚实的降级。

use fs_rdpproto::Rect;

/// 积压矩形数达到这个值就退化为全屏一发。
pub const MAX_PENDING_RECTS: usize = 256;

/// 待发矩形集合。
///
/// `add` 贪心合并：新矩形与已有集合**任一**矩形相交（或相邻贴边）就并入那一个。
/// 相交判定用外扩 1 像素的宽松口径——两个只差 1 像素的矩形，包围盒几乎不增大，
/// 而分开发要两倍的头部开销与两次 IPC。
#[derive(Debug, Default, Clone)]
pub struct DirtyRects {
    rects: Vec<Rect>,
    /// 桌面尺寸（收到 Connected 后设定；用于全屏退化）
    screen: Option<(u16, u16)>,
}

impl DirtyRects {
    pub fn new(screen: (u16, u16)) -> Self {
        Self {
            rects: Vec::new(),
            screen: Some(screen),
        }
    }

    /// 加一块脏区。
    pub fn add(&mut self, r: Rect) {
        if r.is_empty() {
            return;
        }
        // 找一个可合并的（相交或贴边），并入后可能需要再与其他块合并——
        // 简化处理：并入第一个命中者即可。贪心不追求最优合并，只追求有界。
        for existing in &mut self.rects {
            if touches(existing, &r) {
                *existing = existing.union(&r);
                return;
            }
        }
        if self.rects.len() >= MAX_PENDING_RECTS {
            // 退化为全屏：清空集合，只留一块。screen 未定时（不该发生）保留原状。
            if let Some((w, h)) = self.screen {
                self.rects.clear();
                self.rects.push(Rect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                });
                return;
            }
        }
        self.rects.push(r);
    }

    /// 有没有待发矩形。帧级背压下「配额用尽」与「没东西可发」是两回事，
    /// 调用方要能分辨（见 engine.rs 的 flush_frames）。
    pub fn has_pending(&self) -> bool {
        !self.rects.is_empty()
    }

    /// 取走全部待发矩形（发送后集合清空）。
    pub fn take(&mut self) -> Vec<Rect> {
        std::mem::take(&mut self.rects)
    }
}

/// 两个矩形是否相交或贴边（外扩 1 像素口径）。
fn touches(a: &Rect, b: &Rect) -> bool {
    // 相交的补集：b 完全在 a 左侧/右侧/上方/下方（且不贴边）。
    let separated =
        b.x + b.width < a.x || a.x + a.width < b.x || b.y + b.height < a.y || a.y + a.height < b.y;
    !separated
}

/// 从 RGBA 帧缓冲里裁出一块矩形的紧凑像素（逐行拷贝、去掉行间 padding）。
///
/// `data` 是整幅图（`stride` 字节/行）；返回 `(w'*h')*4` 字节的连续块，
/// 前端 `putImageData` 直接可用。**矩形先钳到帧内**：宽 `w' = min(w, frame_w -
/// x)`、高同理——返回块的实际尺寸可能与请求的 `rect` 不同，调用方以返回块
/// 长度对应的尺寸为准（引擎构造 Frame 头时用钳后的 rect）。
pub fn clamp_to_frame(data_len: usize, stride: usize, rect: &Rect) -> Rect {
    if stride == 0 {
        return Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
    }
    let frame_w = (stride / 4) as u16;
    let frame_h = (data_len / stride) as u16;
    let x = rect.x.min(frame_w);
    let y = rect.y.min(frame_h);
    Rect {
        x,
        y,
        width: rect.width.min(frame_w.saturating_sub(x)),
        height: rect.height.min(frame_h.saturating_sub(y)),
    }
}

/// [`clamp_to_frame`] 之后按钳定的矩形逐行拷贝。
pub fn extract_rgba(data: &[u8], stride: usize, rect: &Rect) -> Vec<u8> {
    let w = rect.width as usize;
    let h = rect.height as usize;
    let mut out = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let start = (rect.y as usize + row) * stride + rect.x as usize * 4;
        let end = start + w * 4;
        if end <= data.len() {
            out.extend_from_slice(&data[start..end]);
        } else {
            // 钳定之后仍越界 = data 与 stride 自相矛盾（上游 bug）：
            // 补零继续，宁可显示坏像素也不 panic——这是渲染路径。
            out.resize(w * h * 4, 0);
            return out;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    /// 相交的两块合并成一块——一次 IPC 帧而不是两次。
    #[test]
    fn overlapping_rects_merge_into_one() {
        let mut d = DirtyRects::new((100, 100));
        d.add(r(10, 10, 20, 20));
        d.add(r(25, 25, 20, 20)); // 与第一块交叠
        assert_eq!(d.take(), vec![r(10, 10, 35, 35)]);
    }

    /// 贴边（不交叠但相邻）也合并：包围盒几乎不增大，省一次帧头。
    #[test]
    fn adjacent_rects_merge_too() {
        let mut d = DirtyRects::new((100, 100));
        d.add(r(0, 0, 10, 10));
        d.add(r(10, 0, 10, 10)); // 右贴边
        assert_eq!(d.take(), vec![r(0, 0, 20, 10)]);
    }

    /// 完全分离的两块**不**合并——合并它们的包围盒会重画大量没变的像素。
    #[test]
    fn far_apart_rects_stay_separate() {
        let mut d = DirtyRects::new((1000, 1000));
        d.add(r(0, 0, 10, 10));
        d.add(r(500, 500, 10, 10));
        assert_eq!(d.rects.len(), 2, "远离的两块不该合并");
    }

    /// **合并式背压的本义**：同一块区域连续更新，只留最后一次的包围盒。
    /// 这正是「后写覆盖前写、丢弃中间态是正确的」的载体测试。
    #[test]
    fn repeated_updates_to_the_same_area_do_not_accumulate() {
        let mut d = DirtyRects::new((1920, 1080));
        for i in 0..1000 {
            d.add(r(100, 100 + i, 50, 5)); // 逐行往下刷
        }
        assert_eq!(
            d.rects.len(),
            1,
            "同一区域的连续更新必须合并，实得 {} 块",
            d.rects.len()
        );
    }

    /// 矩形数超上限 → 退化为全屏一块。
    #[test]
    fn too_many_rects_degrade_to_one_fullscreen() {
        let mut d = DirtyRects::new((800, 600));
        // 撒互不相交的小块（间隔 10 像素，触不到一起）
        for i in 0..=MAX_PENDING_RECTS + 5 {
            let x = ((i % 40) * 20) as u16;
            let y = ((i / 40) * 20) as u16;
            d.add(r(x, y, 5, 5));
        }
        assert!(d.rects.len() <= MAX_PENDING_RECTS);
        let taken = d.take();
        // 退化后应恰好是全屏
        assert_eq!(taken.len(), 1, "超限应退化为一块");
        assert_eq!(taken[0], r(0, 0, 800, 600));
    }

    /// 空矩形被丢弃，不占集合也不产生帧。
    #[test]
    fn empty_rects_are_dropped() {
        let mut d = DirtyRects::new((100, 100));
        d.add(r(0, 0, 0, 10));
        d.add(r(0, 0, 10, 0));
        assert!(d.rects.is_empty());
    }

    /// extract：带 stride 的裁剪，行间 padding 不进输出。
    #[test]
    fn extract_skips_row_padding() {
        // 4x2 图，stride 20（4 像素 ×4B = 16B 数据 + 4B padding）
        let mut data = vec![0u8; 40];
        for y in 0..2 {
            for x in 0..4 {
                data[y * 20 + x * 4] = (x + y * 10) as u8;
            }
        }
        let out = extract_rgba(&data, 20, &r(1, 0, 2, 2));
        // 第一行：像素 (1,0),(2,0) → 首字节 1, 2
        assert_eq!(out[0], 1);
        assert_eq!(out[4], 2);
        // 第二行：像素 (1,1),(2,1) → 首字节 11, 12
        assert_eq!(out[8], 11);
        assert_eq!(out[12], 12);
        assert_eq!(out.len(), 2 * 2 * 4);
    }

    /// extract：越界钳制不 panic（IronRDP 的矩形理论上在界内，防御性）。
    #[test]
    fn extract_clamps_out_of_bounds_without_panicking() {
        let data = vec![9u8; 16]; // 名义上 4x1
        let clamped = clamp_to_frame(data.len(), 16, &r(2, 0, 100, 5));
        assert_eq!(
            (clamped.width, clamped.height),
            (2, 1),
            "宽度钳到 2、高度钳到 1"
        );
        let out = extract_rgba(&data, 16, &clamped);
        assert_eq!(out.len(), 2 * 4);
        assert!(out.iter().all(|&b| b == 9));
    }
}
