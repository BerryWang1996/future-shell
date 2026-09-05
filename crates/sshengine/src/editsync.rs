//! 外部编辑器关联的回传判定（M4a，Xftp「用关联程序编辑」对标）。
//!
//! 流程：远端文件下载到暂存区 → 交给本地编辑器 → 本地文件变了就回传。
//! 唯一真正难的地方是**回传前那一次判断**，本模块只做这件事，且做成纯函数：
//! 它决定「要不要覆盖远端」，而覆盖别人刚改的文件是不可逆的。
//!
//! 判据是两组元数据的比较，而不是一个布尔标记：
//!   · 本地暂存文件与「我们写下去时」的状态比 → 用户到底存过没有；
//!   · 远端文件与「我们下载时」的状态比 → 这中间有没有别人改过。
//!
//! 后者是这一项的要害。少了它，两个人同时编辑同一个配置文件时，后保存的一方会把
//! 前一方的修改**静默**抹掉——没有报错、没有提示，界面上一切正常。

use serde::Serialize;

/// 一个被编辑文件的元数据快照（size + mtime）。
///
/// 为什么不用内容哈希：编辑循环里要对每个打开的文件反复轮询，为此重下一遍远端内容
/// 太贵；而 size+mtime 的判据方向是**安全**的——它可能多报一次冲突（mtime 变了而内容
/// 没变，例如 `touch`），但不会漏报「内容变了」这种真冲突。多问一次的代价是一个对话框，
/// 漏问一次的代价是别人的工作被抹掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub size: u64,
    pub mtime: i64,
}

/// 回传判定的结论。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Decision {
    /// 本地没动过：什么都不做（编辑器打开了又关掉是常态，不该产生一次传输）。
    NoLocalChange,
    /// 本地改了、远端没动：可以直接回传。
    Upload,
    /// 本地改了、远端也变了：**必须问过用户**。载荷带上两边的快照，供 UI 讲清冲突。
    Conflict {
        remote_at_open: Snapshot,
        remote_now: Snapshot,
    },
    /// 远端文件不见了（被删/被移走）。同样不能默默重建——用户可能正是想删掉它。
    RemoteGone,
}

/// 回传判定。三个入参分别是：
///   · `local_at_open`：我们把内容写进暂存区之后立刻取的快照；
///   · `local_now`：现在的暂存文件快照（`None` = 暂存文件不见了，视为没改过——
///     用户可能清了临时目录，那不是「要回传」的信号）；
///   · `remote_at_open` / `remote_now`：下载时与现在的远端快照（后者 `None` = 远端已消失）。
pub fn decide(
    local_at_open: Snapshot,
    local_now: Option<Snapshot>,
    remote_at_open: Snapshot,
    remote_now: Option<Snapshot>,
) -> Decision {
    let Some(local_now) = local_now else {
        return Decision::NoLocalChange;
    };
    if local_now == local_at_open {
        return Decision::NoLocalChange;
    }
    let Some(remote_now) = remote_now else {
        return Decision::RemoteGone;
    };
    if remote_now != remote_at_open {
        return Decision::Conflict {
            remote_at_open,
            remote_now,
        };
    }
    Decision::Upload
}

#[cfg(test)]
mod tests {
    use super::*;

    const L0: Snapshot = Snapshot {
        size: 10,
        mtime: 1000,
    };
    const R0: Snapshot = Snapshot {
        size: 10,
        mtime: 2000,
    };

    #[test]
    fn no_local_change_means_no_transfer() {
        // 打开又关掉，什么都没存：不该产生传输
        assert_eq!(decide(L0, Some(L0), R0, Some(R0)), Decision::NoLocalChange);
        // 暂存文件被清掉（用户清临时目录）：同样不是「要回传」的信号
        assert_eq!(decide(L0, None, R0, Some(R0)), Decision::NoLocalChange);
    }

    #[test]
    fn local_saved_and_remote_untouched_uploads() {
        let saved = Snapshot {
            size: 12,
            mtime: 1005,
        };
        assert_eq!(decide(L0, Some(saved), R0, Some(R0)), Decision::Upload);
        // 只有 mtime 变（同尺寸的原地保存，最常见的一种）也算改过
        let touched_same_size = Snapshot {
            size: 10,
            mtime: 1005,
        };
        assert_eq!(
            decide(L0, Some(touched_same_size), R0, Some(R0)),
            Decision::Upload
        );
        // 只有 size 变（某些编辑器保存后 mtime 分辨率不足）也算改过
        let same_mtime_diff_size = Snapshot {
            size: 11,
            mtime: 1000,
        };
        assert_eq!(
            decide(L0, Some(same_mtime_diff_size), R0, Some(R0)),
            Decision::Upload
        );
    }

    #[test]
    fn remote_changed_is_a_conflict_not_an_overwrite() {
        let saved = Snapshot {
            size: 12,
            mtime: 1005,
        };
        let remote_changed = Snapshot {
            size: 99,
            mtime: 2500,
        };
        assert_eq!(
            decide(L0, Some(saved), R0, Some(remote_changed)),
            Decision::Conflict {
                remote_at_open: R0,
                remote_now: remote_changed
            },
            "远端在编辑期间被改过 → 必须问用户；静默覆盖会抹掉别人的修改"
        );
        // 远端只有 mtime 变（内容可能没变）也报冲突：宁可多问一次对话框，
        // 不可漏问一次而抹掉别人的工作。
        let touched = Snapshot {
            size: 10,
            mtime: 2001,
        };
        assert!(matches!(
            decide(L0, Some(saved), R0, Some(touched)),
            Decision::Conflict { .. }
        ));
    }

    #[test]
    fn remote_gone_is_reported_not_recreated() {
        let saved = Snapshot {
            size: 12,
            mtime: 1005,
        };
        assert_eq!(decide(L0, Some(saved), R0, None), Decision::RemoteGone);
    }

    #[test]
    fn remote_state_is_irrelevant_when_local_never_changed() {
        // 本地没存过 → 即便远端天翻地覆也不该冒出冲突对话框（用户什么都没做）
        let remote_changed = Snapshot {
            size: 99,
            mtime: 9999,
        };
        assert_eq!(
            decide(L0, Some(L0), R0, Some(remote_changed)),
            Decision::NoLocalChange
        );
        assert_eq!(decide(L0, Some(L0), R0, None), Decision::NoLocalChange);
    }
}
