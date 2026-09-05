//! RDPDR 的桥接后端（RDPDR 批）：把上游 `ServerDriveIoRequest` 翻译成
//! `fs_rdpproto::FsOp` 发给主程序，应答回来后再组装成 MS-RDPEFS 响应。
//!
//! # 为什么文件 IO 不在这里做
//!
//! 与剪贴板同一条纪律（见 clipboard.rs 模块头）：helper 是解析远端不可信字节的
//! 沙箱进程，**授权、路径闸、只读闸、审计全在主程序**。helper 只做翻译——
//! 即便它被协议字节打穿，拿到的也只是「(句柄号, 偏移, 长度)」这样的语义形状，
//! 没有路径，更没有越界的可能。
//!
//! # 「同步 trait + 异步应答」怎么接
//!
//! `RdpdrBackend::handle_drive_io_request` 是同步的，而主程序的应答要等管道
//! 往返。上游给的正解（trait 文档原文）：返回空 `Vec` = 暂缓，另行排队。于是：
//! backend 把 IRP 的完成信息（completion_id 等，应答时组装响应要用）挂进
//! `pending` 表、发上行、返回空；engine 收到 `DriveIoResult` 时查表组装响应，
//! 经 SVC 通道发出。`pending` 表由 backend 与 engine 共享（`Arc<Mutex>`）。
//!
//! # 分页列目录
//!
//! MS-RDPEFS 的目录枚举是**逐条拉**的：服务器反复对同一 FileId 发
//! QueryDirectory（path 为空 = 「下一条」），客户端每次回一条，回
//! `STATUS_NO_MORE_FILES` 终止。主程序一次列全（`ListDir`），helper 按打开的
//! 目录句柄缓存快照、逐条应答——主程序不必理解 NT 的分页语义，这正是
//! 「语义翻译收窄在 helper」的用意。

use fs_rdpproto::{FromHelper, FsOp, ToHelper};
use ironrdp_rdpdr::pdu::efs::{
    ClientDriveQueryVolumeInformationResponse, DeviceCloseResponse, DeviceCreateResponse,
    DeviceIoRequest, DeviceIoResponse, DeviceReadResponse, DeviceWriteResponse,
    FileDirectoryInformation, FileFsVolumeInformation, FileSystemInformationClass, Information,
    NtStatus,
};
use ironrdp_rdpdr::pdu::RdpdrPdu;
use ironrdp_svc::SvcMessage;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// 上行通道的类型（engine 的 `ToMain` 里的 header 用）。
pub type ToMainTx = mpsc::UnboundedSender<crate::engine::ToMain>;

/// 一个在途 IRP 的完成信息（应答到达时组装 MS-RDPEFS 响应用）。
struct Pending {
    device_io_request: DeviceIoRequest,
    /// 这条 IRP 对应哪种应答形状。
    kind: PendingKind,
}

enum PendingKind {
    Create,
    Read,
    Write,
    Close,
    ListDir,
}

/// 按目录句柄缓存的枚举快照（分页状态）。
struct DirSnapshot {
    entries: Vec<FileDirectoryInformation>,
    offset: usize,
}

/// 共享状态：在途表 + 目录快照 + 主程序上行。
///
/// Debug 手写：字段全是容器，手写比 derive 出来的好看不了多少，
/// 而 mpsc::Sender 不实现 Debug——derive 直接不可行。
impl std::fmt::Debug for DriveBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriveBridge")
            .field("pending", &self.pending.lock().unwrap().len())
            .field("dirs", &self.dirs.lock().unwrap().len())
            .finish()
    }
}

pub struct DriveBridge {
    to_main: ToMainTx,
    pending: Mutex<HashMap<u64, Pending>>,
    dirs: Mutex<HashMap<u32, Arc<Mutex<DirSnapshot>>>>,
    next_id: AtomicU64,
    /// 主程序视角的设备号（MountDrive 下来的 u8）→ RDPDR 设备 id（u32）。
    /// 两个世界各用各的编号：u8 是我们协议的，u32 是 MS-RDPEFS 线上的，
    /// 在挂载时由 helper 对上（`device_map`）。
    device_map: Mutex<HashMap<u32, u8>>,
    pub rdpdr_to_proto: Mutex<HashMap<u8, u32>>,
}

impl DriveBridge {
    pub fn new(to_main: ToMainTx) -> Arc<Self> {
        Arc::new(Self {
            to_main,
            pending: Mutex::new(HashMap::new()),
            dirs: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            device_map: Mutex::new(HashMap::new()),
            rdpdr_to_proto: Mutex::new(HashMap::new()),
        })
    }

    /// 拿走「协议设备号 → 线上设备号」的映射（卸载时用，用后即删）。
    pub fn take_wire_id(&self, proto_device: u8) -> Option<u32> {
        let w = self.rdpdr_to_proto.lock().unwrap().remove(&proto_device);
        if let Some(w) = w {
            self.device_map.lock().unwrap().remove(&w);
        }
        w
    }

    /// 主程序挂载指令 → RDPDR 设备公告。返回要发出的公告消息
    /// （由 engine 经 `Rdpdr::add_drive` 路径发出——那里才持有 processor）。
    pub fn register_device(&self, rdpdr_device: u32, proto_device: u8) {
        self.device_map
            .lock()
            .unwrap()
            .insert(rdpdr_device, proto_device);
        self.rdpdr_to_proto
            .lock()
            .unwrap()
            .insert(proto_device, rdpdr_device);
    }

    /// 上行一条 DriveIo（带翻译后的语义形状）。
    fn send_up(
        &self,
        device_io_request: &DeviceIoRequest,
        op: FsOp,
        body: Vec<u8>,
        kind: PendingKind,
    ) {
        let device = self
            .device_map
            .lock()
            .unwrap()
            .get(&device_io_request.device_id)
            .copied()
            .unwrap_or(0);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.pending.lock().unwrap().insert(
            id,
            Pending {
                device_io_request: clone_io_request(device_io_request),
                kind,
            },
        );
        let _ = self.to_main.send(crate::engine::ToMain {
            header: FromHelper::DriveIo { id, device, op },
            body,
        });
    }

    /// 主程序的应答到达（engine 调）：组装并发送 MS-RDPEFS 响应。
    /// 返回要发给 SVC 的消息（空 = 没有对应的在途 IRP：会话关闭竞态，丢弃）。
    pub fn complete(&self, r: ToHelper, body: Vec<u8>) -> Vec<SvcMessage> {
        let ToHelper::DriveIoResult {
            id,
            status,
            handle,
            n,
            is_dir,
        } = r
        else {
            return Vec::new();
        };
        let Some(p) = self.pending.lock().unwrap().remove(&id) else {
            return Vec::new();
        };
        let nt = NtStatus::from(status);
        let req = p.device_io_request;
        match p.kind {
            PendingKind::Create => {
                // 打开目录：为后续的目录枚举建快照槽（等 ListDir 来填）。
                if status == 0 && is_dir {
                    if let Some(h) = handle {
                        self.dirs.lock().unwrap().insert(
                            h as u32,
                            Arc::new(Mutex::new(DirSnapshot {
                                entries: Vec::new(),
                                offset: 0,
                            })),
                        );
                    }
                }
                let file_id = handle.unwrap_or(0) as u32;
                let information = if n > 0 {
                    Information::FILE_SUPERSEDED
                } else {
                    Information::FILE_OPENED
                };
                vec![SvcMessage::from(RdpdrPdu::DeviceCreateResponse(
                    DeviceCreateResponse {
                        device_io_reply: DeviceIoResponse::new(req, nt),
                        file_id,
                        information,
                    },
                ))]
            }
            PendingKind::Read => {
                vec![SvcMessage::from(RdpdrPdu::DeviceReadResponse(
                    DeviceReadResponse {
                        device_io_reply: DeviceIoResponse::new(req, nt),
                        read_data: body,
                    },
                ))]
            }
            PendingKind::Write => vec![SvcMessage::from(RdpdrPdu::DeviceWriteResponse(
                DeviceWriteResponse {
                    device_io_reply: DeviceIoResponse::new(req, nt),
                    length: n as u32,
                },
            ))],
            PendingKind::Close => {
                self.dirs.lock().unwrap().remove(&req.file_id);
                vec![SvcMessage::from(RdpdrPdu::DeviceCloseResponse(
                    DeviceCloseResponse {
                        device_io_response: DeviceIoResponse::new(req, nt),
                    },
                ))]
            }
            PendingKind::ListDir => {
                // 应答体 = 全量行；这里转成快照。本条 IRP 的应答 = 第一条目录项。
                let snap = self.dirs.lock().unwrap().get(&req.file_id).cloned();
                let Some(snap) = snap else {
                    return vec![SvcMessage::from(RdpdrPdu::DeviceCloseResponse(
                        DeviceCloseResponse {
                            device_io_response: DeviceIoResponse::new(req, NtStatus::NOT_SUPPORTED),
                        },
                    ))];
                };
                {
                    let mut s = snap.lock().unwrap();
                    s.entries = parse_listing(&body);
                    s.offset = 0;
                }
                self.emit_dir_entry(req, snap)
            }
        }
    }

    /// 从快照弹一条目录项应答（没有下一条时 NO_MORE_FILES）。
    fn emit_dir_entry(
        &self,
        req: DeviceIoRequest,
        snap: Arc<Mutex<DirSnapshot>>,
    ) -> Vec<SvcMessage> {
        let mut s = snap.lock().unwrap();
        if s.offset >= s.entries.len() {
            return vec![SvcMessage::from(RdpdrPdu::DeviceCloseResponse(
                DeviceCloseResponse {
                    device_io_response: DeviceIoResponse::new(req, NtStatus::NO_MORE_FILES),
                },
            ))];
        }
        let entry = s.entries[s.offset].clone();
        s.offset += 1;
        vec![SvcMessage::from(
            RdpdrPdu::ClientDriveQueryDirectoryResponse(
                ironrdp_rdpdr::pdu::efs::ClientDriveQueryDirectoryResponse {
                    device_io_reply: DeviceIoResponse::new(req, NtStatus::SUCCESS),
                    buffer: Some(ironrdp_rdpdr::pdu::efs::FileInformationClass::Directory(
                        entry,
                    )),
                },
            ),
        )]
    }

    /// 服务器对同一目录的「下一条」拉取（path 为空）：不走主程序，直接从快照弹。
    pub fn next_dir_entry(&self, req: DeviceIoRequest) -> Vec<SvcMessage> {
        let snap = self.dirs.lock().unwrap().get(&req.file_id).cloned();
        match snap {
            Some(s) => self.emit_dir_entry(req, s),
            None => vec![SvcMessage::from(RdpdrPdu::DeviceCloseResponse(
                DeviceCloseResponse {
                    device_io_response: DeviceIoResponse::new(req, NtStatus::NOT_SUPPORTED),
                },
            ))],
        }
    }
}

/// backend（同步 trait）：只做翻译 + 挂起。
#[derive(Debug)]
pub struct PipeDriveBackend {
    pub bridge: Arc<DriveBridge>,
}
ironrdp_core::impl_as_any!(PipeDriveBackend);

impl ironrdp_rdpdr::RdpdrBackend for PipeDriveBackend {
    fn handle_server_device_announce_response(
        &mut self,
        pdu: ironrdp_rdpdr::pdu::efs::ServerDeviceAnnounceResponse,
    ) -> ironrdp_pdu::PduResult<()> {
        tracing::debug!(?pdu, "rdpdr: server announce response");
        Ok(())
    }

    fn handle_scard_call(
        &mut self,
        req: ironrdp_rdpdr::pdu::efs::DeviceControlRequest<ironrdp_rdpdr::pdu::esc::ScardIoCtlCode>,
        _call: ironrdp_rdpdr::pdu::esc::ScardCall,
    ) -> ironrdp_pdu::PduResult<()> {
        // 智能卡重定向不做（声明的设备表里就没有智能卡，这条理论上到不了）。
        let _ = req;
        Ok(())
    }

    fn handle_drive_io_request(
        &mut self,
        req: ironrdp_rdpdr::pdu::efs::ServerDriveIoRequest,
    ) -> ironrdp_pdu::PduResult<Vec<SvcMessage>> {
        use ironrdp_rdpdr::pdu::efs::ServerDriveIoRequest as R;

        // 卷信息：helper 本地应答（不需要看磁盘——盘标是我们起的名字）。
        if let R::ServerDriveQueryVolumeInformationRequest(v) = &req {
            return Ok(vec![SvcMessage::from(
                RdpdrPdu::ClientDriveQueryVolumeInformationResponse(
                    ClientDriveQueryVolumeInformationResponse::new(
                        clone_io_request(&v.device_io_request),
                        NtStatus::SUCCESS,
                        Some(FileSystemInformationClass::FileFsVolumeInformation(
                            FileFsVolumeInformation {
                                volume_creation_time: 0,
                                volume_serial_number: 0x0046_5353, // "FSS"
                                supports_objects: ironrdp_rdpdr::pdu::efs::Boolean::False,
                                volume_label: "RDP 共享".into(),
                            },
                        )),
                    ),
                ),
            )]);
        }

        match req {
            R::ServerCreateDriveRequest(c) => {
                let directory = c
                    .create_options
                    .contains(ironrdp_rdpdr::pdu::efs::CreateOptions::FILE_DIRECTORY_FILE);
                let (write, disposition) =
                    translate_create(c.desired_access, c.create_disposition, directory);
                let path = c.path.replace('\\', "/");
                // 根路径（空或 `.`）= 盘符本身：当「列根目录」的预打开。
                let path = if path.is_empty() {
                    ".".to_string()
                } else {
                    path
                };
                self.bridge.send_up(
                    &c.device_io_request,
                    FsOp::Create {
                        path,
                        write,
                        disposition,
                    },
                    Vec::new(),
                    PendingKind::Create,
                );
                Ok(Vec::new()) // 暂缓：应答从主程序回来后由 engine 发出
            }
            R::DeviceReadRequest(r) => {
                self.bridge.send_up(
                    &r.device_io_request,
                    FsOp::Read {
                        handle: r.device_io_request.file_id as u64,
                        offset: r.offset,
                        len: r.length,
                    },
                    Vec::new(),
                    PendingKind::Read,
                );
                Ok(Vec::new())
            }
            R::DeviceWriteRequest(w) => {
                self.bridge.send_up(
                    &w.device_io_request,
                    FsOp::Write {
                        handle: w.device_io_request.file_id as u64,
                        offset: w.offset,
                    },
                    w.write_data,
                    PendingKind::Write,
                );
                Ok(Vec::new())
            }
            R::DeviceCloseRequest(cl) => {
                self.bridge.send_up(
                    &cl.device_io_request,
                    FsOp::Close {
                        handle: cl.device_io_request.file_id as u64,
                    },
                    Vec::new(),
                    PendingKind::Close,
                );
                Ok(Vec::new())
            }
            R::ServerDriveQueryDirectoryRequest(q) => {
                if q.path.is_empty() {
                    // 「下一条」：从快照弹，不惊动主程序。
                    Ok(self
                        .bridge
                        .next_dir_entry(clone_io_request(&q.device_io_request)))
                } else {
                    self.bridge.send_up(
                        &q.device_io_request,
                        FsOp::ListDir {
                            path: q.path.replace('\\', "/"),
                        },
                        Vec::new(),
                        PendingKind::ListDir,
                    );
                    Ok(Vec::new())
                }
            }
            // 其余 IRP（QueryInfo/SetInfo/Lock/NotifyChange/DeviceControl）：
            // MVP 不做——同步回 NOT_SUPPORTED。真 Windows Explorer 对这些
            // 有降级路径（属性页显示不全/改名不支持），盘能开、能列、能读写。
            other => {
                let req = io_request_of(&other);
                Ok(vec![SvcMessage::from(RdpdrPdu::DeviceCloseResponse(
                    DeviceCloseResponse {
                        device_io_response: DeviceIoResponse::new(req, NtStatus::NOT_SUPPORTED),
                    },
                ))])
            }
        }
    }
}

/// 从任意 ServerDriveIoRequest 取其 DeviceIoRequest（NOT_SUPPORTED 应答要用）。
fn io_request_of(r: &ironrdp_rdpdr::pdu::efs::ServerDriveIoRequest) -> DeviceIoRequest {
    use ironrdp_rdpdr::pdu::efs::ServerDriveIoRequest as R;
    let d = match r {
        R::ServerCreateDriveRequest(x) => &x.device_io_request,
        R::ServerDriveQueryInformationRequest(x) => &x.device_io_request,
        R::DeviceCloseRequest(x) => &x.device_io_request,
        R::ServerDriveQueryDirectoryRequest(x) => &x.device_io_request,
        R::ServerDriveNotifyChangeDirectoryRequest(x) => &x.device_io_request,
        R::ServerDriveQueryVolumeInformationRequest(x) => &x.device_io_request,
        R::DeviceControlRequest(x) => &x.header,
        R::DeviceReadRequest(x) => &x.device_io_request,
        R::DeviceWriteRequest(x) => &x.device_io_request,
        R::ServerDriveSetInformationRequest(x) => &x.device_io_request,
        R::ServerDriveLockControlRequest(x) => &x.device_io_request,
    };
    clone_io_request(d)
}

/// DeviceIoRequest 没实现 Clone——手抄一份（四个字段，全 pub）。
fn clone_io_request(d: &DeviceIoRequest) -> DeviceIoRequest {
    DeviceIoRequest {
        device_id: d.device_id,
        file_id: d.file_id,
        completion_id: d.completion_id,
        major_function: d.major_function,
        minor_function: d.minor_function,
    }
}

/// 主程序 ListDir 的行格式 → FileDirectoryInformation。
/// 行：`名字\t类型(d/f)\t尺寸\tmtime_unix_ms`。mtime 传 0（NT FILETIME 换算
/// 留给需要它的那一档——资源管理器没有它也能列目录）。
fn parse_listing(body: &[u8]) -> Vec<FileDirectoryInformation> {
    let text = String::from_utf8_lossy(body);
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        let (Some(name), Some(kind), Some(size), _) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let is_dir = kind == "d";
        out.push(FileDirectoryInformation::new(
            0,
            0,
            0,
            0,
            size.parse::<i64>().unwrap_or(0),
            if is_dir {
                ironrdp_rdpdr::pdu::efs::FileAttributes::FILE_ATTRIBUTE_DIRECTORY
            } else {
                ironrdp_rdpdr::pdu::efs::FileAttributes::FILE_ATTRIBUTE_NORMAL
            },
            name.to_string(),
        ));
    }
    out
}

/// MS-RDPEFS 的 Create IRP → 我们的语义形状（写意图 + disposition）。
///
/// 写意图的判定要**宁可报写**：把一次写当读放过去，主程序就当读处理
/// （共享只读时拦不住），那才是漏。翻译错了没有任何协议层信号——
/// 只有真 Windows 上「只读共享居然能改文件」这种最坏的发现方式，所以纯函数钉死。
pub fn translate_create(
    da: ironrdp_rdpdr::pdu::efs::DesiredAccess,
    disp: ironrdp_rdpdr::pdu::efs::CreateDisposition,
    directory: bool,
) -> (bool, u8) {
    use ironrdp_rdpdr::pdu::efs::{CreateDisposition, DesiredAccess};
    // FILE_OPEN_IF 是双面的：对**目录**（资源管理器翻文件夹全用它）是纯读——
    // 目录已存在，「或建新」那半不会发生；对**文件**它随时可能创建，必须按写
    // 对待（只读共享上「打开或建新」的另一半就是建新）。不区分的话两头都错：
    // 全判写 → 只读共享连文件夹都翻不开；全判读 → 只读闸对「顺手建个文件」漏。
    let write = da.intersects(
        DesiredAccess::FILE_WRITE_DATA_OR_FILE_ADD_FILE
            | DesiredAccess::FILE_APPEND_DATA_OR_FILE_ADD_SUBDIRECTORY
            | DesiredAccess::FILE_WRITE_EA
            | DesiredAccess::FILE_WRITE_ATTRIBUTES
            | DesiredAccess::DELETE,
    ) || matches!(
        disp,
        CreateDisposition::FILE_SUPERSEDE
            | CreateDisposition::FILE_CREATE
            | CreateDisposition::FILE_OVERWRITE
            | CreateDisposition::FILE_OVERWRITE_IF
    ) || (disp == CreateDisposition::FILE_OPEN_IF && !directory);
    let disposition = match disp {
        CreateDisposition::FILE_OPEN => 0,    // 打开已有
        CreateDisposition::FILE_CREATE => 1,  // 建新
        CreateDisposition::FILE_OPEN_IF => 3, // 打开或建新
        CreateDisposition::FILE_SUPERSEDE | CreateDisposition::FILE_OVERWRITE => 2, // 打开或截断
        CreateDisposition::FILE_OVERWRITE_IF => 3,
        _ => 0,
    };
    (write, disposition)
}

#[cfg(test)]
mod tests {
    use super::translate_create;
    use ironrdp_rdpdr::pdu::efs::{CreateDisposition, DesiredAccess};

    const DA_READ: DesiredAccess = DesiredAccess::FILE_READ_DATA_OR_FILE_LIST_DIRECTORY;

    #[test]
    fn plain_open_is_not_a_write() {
        assert_eq!(
            translate_create(DA_READ, CreateDisposition::FILE_OPEN, false),
            (false, 0)
        );
        // 目录的 OPEN_IF（资源管理器翻文件夹的形态）也是读
        assert_eq!(
            translate_create(DA_READ, CreateDisposition::FILE_OPEN_IF, true),
            (false, 3)
        );
    }

    /// FILE_OPEN 被翻成「打开或建新」（D7 变异）的后果：远端 merely 打开一个
    /// 不存在的名字，我们这边把它**创建**出来——资源管理器的地址栏试探都能
    /// 在共享里凭空生文件。
    #[test]
    fn file_open_never_creates() {
        for disp in [
            CreateDisposition::FILE_OPEN,
            CreateDisposition::FILE_SUPERSEDE,
            CreateDisposition::FILE_CREATE,
            CreateDisposition::FILE_OPEN_IF,
            CreateDisposition::FILE_OVERWRITE,
            CreateDisposition::FILE_OVERWRITE_IF,
        ] {
            let (w, d) = translate_create(DA_READ, disp, false);
            // disposition 语义表逐格钉死（file 视角）
            let want = match disp {
                CreateDisposition::FILE_OPEN => (false, 0),
                CreateDisposition::FILE_CREATE => (true, 1),
                CreateDisposition::FILE_OPEN_IF => (true, 3),
                CreateDisposition::FILE_SUPERSEDE => (true, 2),
                CreateDisposition::FILE_OVERWRITE => (true, 2),
                CreateDisposition::FILE_OVERWRITE_IF => (true, 3),
                _ => unreachable!(),
            };
            assert_eq!((w, d), want, "{disp:?}");
        }
    }

    /// D8 变异（FILE_WRITE_ATTRIBUTES/DELETE 漏判）的后果：以「改属性/删除」
    /// 为目的的打开被当读放行——只读闸对它们失效。
    #[test]
    fn every_write_access_flags_is_classified_as_write() {
        for da in [
            DesiredAccess::FILE_WRITE_DATA_OR_FILE_ADD_FILE,
            DesiredAccess::FILE_APPEND_DATA_OR_FILE_ADD_SUBDIRECTORY,
            DesiredAccess::FILE_WRITE_EA,
            DesiredAccess::FILE_WRITE_ATTRIBUTES,
            DesiredAccess::DELETE,
        ] {
            let name = format!("{da:?}");
            let (w, _) = translate_create(da, CreateDisposition::FILE_OPEN, false);
            assert!(w, "{name} 应判为写");
        }
        // 读位不带写
        let (w, _) = translate_create(
            DesiredAccess::FILE_READ_DATA_OR_FILE_LIST_DIRECTORY | DesiredAccess::FILE_READ_EA,
            CreateDisposition::FILE_OPEN,
            true,
        );
        assert!(!w);
    }
}
