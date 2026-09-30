//! 线格式编解码。
//!
//! 这一层是主程序与 helper 进程**唯一**共享的可执行逻辑，两边各编一份跑。
//! 它错了的表现是「两个进程各说各话」，而那种故障在运行时看起来像
//! 「RDP 莫名其妙连不上」——离真因很远。所以判据尽量落在**边界**上：
//! 收不全、粘包、长度上限、变体与体的有无。

use fs_rdpproto::*;

fn params() -> ConnectParams {
    ConnectParams {
        server_name: "win-01".into(),
        username: "administrator".into(),
        domain: String::new(),
        password: "hunter2".into(),
        width: 1280,
        height: 800,
        keyboard_layout: 0x0409,
    }
}

/// 往返：编出去再解回来，逐字相等。
#[test]
fn a_header_only_message_round_trips() {
    let mut buf = Vec::new();
    let msg = ToHelper::Connect(params());
    encode(&msg, &[], &mut buf);
    let (pkt, used) = decode::<ToHelper>(&buf).unwrap().expect("该解得出");
    assert_eq!(used, buf.len(), "consumed 与实际长度不符");
    assert_eq!(pkt.header, msg);
    assert!(pkt.body.is_empty());
}

/// 带体的往返：**像素不进 JSON**，故体必须逐字节原样穿过。
#[test]
fn a_message_with_a_body_round_trips_byte_for_byte() {
    // 刻意含 0x00、0xFF 与非 UTF-8 序列，外加 JSON 的元字符：
    // 这些正是「不小心把体当字符串处理了」会损坏的字节。
    let body: Vec<u8> = vec![0x00, 0xFF, 0x80, 0xC0, 0x7B, 0x22, 0x5C, 0x0A];
    let mut buf = Vec::new();
    let msg = FromHelper::Frame {
        rect: Rect {
            x: 10,
            y: 20,
            width: 2,
            height: 1,
        },
        format: PixelFormat::Rgba,
    };
    encode(&msg, &body, &mut buf);
    let (pkt, _) = decode::<FromHelper>(&buf).unwrap().unwrap();
    assert_eq!(pkt.header, msg);
    assert_eq!(pkt.body, body, "体被改动了——它必须逐字节原样穿过");
}

/// 收不全时返回 `Ok(None)` 而不是错——这是流式读取的正常状态。
///
/// **逐字节喂**：每一个前缀都必须是「还没收全」，直到最后一字节才成。
/// 只测一个中间长度太松——那样「只要长度 ≥ 8 就尝试解析」的实现也能过。
#[test]
fn every_short_prefix_is_incomplete_not_an_error() {
    let mut buf = Vec::new();
    encode(&ToHelper::NetIn, &[1, 2, 3, 4, 5], &mut buf);
    for n in 0..buf.len() {
        let got = decode::<ToHelper>(&buf[..n]);
        assert_eq!(got, Ok(None), "前 {n} 字节应当是「还没收全」，实得 {got:?}");
    }
    assert!(
        decode::<ToHelper>(&buf).unwrap().is_some(),
        "收全了该解得出"
    );
}

/// 粘包：一次读到两条，第一条的 `consumed` 必须精确到边界。
#[test]
fn two_messages_in_one_buffer_split_at_the_exact_boundary() {
    let mut buf = Vec::new();
    encode(&ToHelper::NetIn, &[9, 9], &mut buf);
    let first_len = buf.len();
    encode(&ToHelper::Shutdown, &[], &mut buf);

    let (a, used) = decode::<ToHelper>(&buf).unwrap().unwrap();
    assert_eq!(used, first_len, "第一条的边界算错了");
    assert_eq!(a.header, ToHelper::NetIn);
    assert_eq!(a.body, vec![9, 9]);

    let (b, used2) = decode::<ToHelper>(&buf[used..]).unwrap().unwrap();
    assert_eq!(b.header, ToHelper::Shutdown);
    assert_eq!(used + used2, buf.len());
}

/// **上限在分配之前检查。**
///
/// 判据不是「报了个错」，而是「在只有 8 字节前缀、体一个字节都没到的情况下
/// 就报错」。反过来写（先等收够再校验）等于让一个声称有 4 GB 体的坏包
/// 把接收循环挂在那里等一辈子——而那个坏包可能来自 helper 解析远端字节时的 bug。
#[test]
fn an_absurd_length_is_rejected_before_waiting_for_the_bytes() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&16u32.to_be_bytes()); // 头 16 字节
    buf.extend_from_slice(&u32::MAX.to_be_bytes()); // 体 4 GB
    assert_eq!(buf.len(), FRAME_PREFIX_LEN, "前提：只喂了前缀");
    assert_eq!(
        decode::<ToHelper>(&buf),
        Err(CodecError::BodyTooLarge { declared: u32::MAX }),
        "只有前缀时就该拒，而不是回 Ok(None) 去等 4 GB"
    );

    let mut buf2 = Vec::new();
    buf2.extend_from_slice(&u32::MAX.to_be_bytes());
    buf2.extend_from_slice(&0u32.to_be_bytes());
    assert_eq!(
        decode::<ToHelper>(&buf2),
        Err(CodecError::HeaderTooLarge { declared: u32::MAX })
    );
}

/// 恰好等于上限要放行——上限是**上限**不是**禁区**。
/// 差一位的错（`>` 写成 `>=`）在这里现形。
#[test]
fn a_length_exactly_at_the_cap_is_accepted() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&16u32.to_be_bytes());
    buf.extend_from_slice(&MAX_BODY_BYTES.to_be_bytes());
    // 只喂前缀：既然长度合法，就该回 Ok(None) 继续等，而不是报错。
    assert_eq!(
        decode::<ToHelper>(&buf),
        Ok(None),
        "恰好等于上限的长度不该被拒"
    );
}

/// **变体与体的有无对不上要单独报。**
///
/// 这一条命中意味着两侧对协议的理解已经分叉，与「单条消息损坏」是完全不同的
/// 严重度——前者说明该换 helper 了。手工拼一个「Shutdown 却带了体」的包来触发。
#[test]
fn a_variant_carrying_an_unexpected_body_is_a_distinct_error() {
    let json = serde_json::to_vec(&ToHelper::Shutdown).unwrap();
    let body = b"unexpected";
    let mut buf = Vec::new();
    buf.extend_from_slice(&(json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(&json);
    buf.extend_from_slice(body);
    assert_eq!(
        decode::<ToHelper>(&buf),
        Err(CodecError::BodyMismatch {
            wants: false,
            got: body.len()
        })
    );
}

/// 反向：声明要体的变体却没给体，同样是分叉。
#[test]
fn a_variant_missing_its_body_is_the_same_kind_of_divergence() {
    let json = serde_json::to_vec(&ToHelper::NetIn).unwrap();
    let mut buf = Vec::new();
    buf.extend_from_slice(&(json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&0u32.to_be_bytes());
    buf.extend_from_slice(&json);
    assert_eq!(
        decode::<ToHelper>(&buf),
        Err(CodecError::BodyMismatch {
            wants: true,
            got: 0
        })
    );
}

/// 坏 JSON 报 `BadHeader`，不 panic。
#[test]
fn a_malformed_header_is_an_error_not_a_panic() {
    let json = b"{not json";
    let mut buf = Vec::new();
    buf.extend_from_slice(&(json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&0u32.to_be_bytes());
    buf.extend_from_slice(json);
    assert!(matches!(
        decode::<ToHelper>(&buf),
        Err(CodecError::BadHeader { .. })
    ));
}

/// `wants_body` 逐个变体点名。
///
/// 不写成「遍历所有变体自动比对」：那需要一份变体清单，而那份清单自己就会漏。
/// 逐个点名的代价是加变体时这里编译不过——那正是想要的。
#[test]
fn the_body_contract_is_pinned_variant_by_variant() {
    assert!(ToHelper::NetIn.wants_body());
    // 阶段 2 剪贴板：通告与数据都走体（大文本不进 JSON）
    assert!(ToHelper::ClipboardOffer.wants_body());
    assert!(ToHelper::ClipboardData.wants_body());
    for m in [
        ToHelper::ClipboardPull,
        ToHelper::Hello { version: 1 },
        ToHelper::Connect(params()),
        ToHelper::NetEof,
        ToHelper::CertVerdict { accept: true },
        ToHelper::Input(InputEvent::MouseMove { x: 1, y: 2 }),
        ToHelper::Resize {
            width: 1,
            height: 1,
        },
        ToHelper::Shutdown,
    ] {
        assert!(!m.wants_body(), "{m:?} 不该带体");
    }

    let rect = Rect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    assert!(FromHelper::NetOut.wants_body());
    assert!(FromHelper::Frame {
        rect,
        format: PixelFormat::Rgba
    }
    .wants_body());
    assert!(FromHelper::ClipboardData.wants_body());
    for m in [
        FromHelper::ClipboardOffer,
        FromHelper::ClipboardRequest,
        FromHelper::Hello { version: 1 },
        FromHelper::CertPresented {
            fingerprint: "aa".into(),
            subject: "s".into(),
            issuer: "i".into(),
            not_after: "2030-01-01T00:00:00Z".into(),
        },
        FromHelper::Connected {
            width: 1,
            height: 1,
        },
        FromHelper::Status {
            message: "x".into(),
        },
        FromHelper::Failed {
            kind: FailureKind::Auth,
            message: "x".into(),
        },
        FromHelper::Closed,
    ] {
        assert!(!m.wants_body(), "{m:?} 不该带体");
    }
}

// ───────────────────────── Rect 几何 ─────────────────────────
//
// 合并是**积压时的丢弃策略**的核心：像素更新后写覆盖前写，
// 把待发矩形合并成包围盒并丢弃中间态是正确的。算错的后果是屏幕上留下
// 不刷新的残影——而残影看起来像「远端卡住了」，会把排查引向完全错误的方向。

#[test]
fn a_union_covers_both_rectangles() {
    let a = Rect {
        x: 10,
        y: 10,
        width: 5,
        height: 5,
    };
    let b = Rect {
        x: 100,
        y: 200,
        width: 3,
        height: 4,
    };
    let u = a.union(&b);
    assert_eq!(
        u,
        Rect {
            x: 10,
            y: 10,
            width: 93,
            height: 194
        }
    );
    // 包围盒必须真的把两个都盖住——逐边核对，不只看宽高。
    assert!(u.x <= a.x && u.x <= b.x);
    assert!(u.y <= a.y && u.y <= b.y);
    assert!(u.x + u.width >= a.x + a.width);
    assert!(u.x + u.width >= b.x + b.width);
    assert!(u.y + u.height >= a.y + a.height);
    assert!(u.y + u.height >= b.y + b.height);
}

#[test]
fn a_union_is_commutative_and_idempotent() {
    let a = Rect {
        x: 3,
        y: 7,
        width: 11,
        height: 13,
    };
    let b = Rect {
        x: 5,
        y: 2,
        width: 4,
        height: 40,
    };
    assert_eq!(a.union(&b), b.union(&a), "合并必须与顺序无关");
    assert_eq!(a.union(&a), a, "自己与自己合并应当不变");
}

/// 包含关系：大的吞小的，结果就是大的。
/// 写错成「取两者宽高的 max」时这里现形（那会得到一个错位的矩形）。
#[test]
fn a_union_with_a_contained_rectangle_is_the_container() {
    let outer = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 100,
    };
    let inner = Rect {
        x: 40,
        y: 40,
        width: 10,
        height: 10,
    };
    assert_eq!(outer.union(&inner), outer);
}

#[test]
fn area_does_not_overflow_at_the_maximum_rectangle() {
    let max = Rect {
        x: 0,
        y: 0,
        width: u16::MAX,
        height: u16::MAX,
    };
    // u16::MAX² = 4_294_836_225 < u32::MAX = 4_294_967_295，恰好放得下。
    assert_eq!(max.area(), 4_294_836_225);
}

#[test]
fn a_zero_dimension_rectangle_is_empty() {
    for r in [
        Rect {
            x: 5,
            y: 5,
            width: 0,
            height: 10,
        },
        Rect {
            x: 5,
            y: 5,
            width: 10,
            height: 0,
        },
    ] {
        assert!(r.is_empty(), "{r:?} 该算空");
        assert_eq!(r.area(), 0);
    }
}
