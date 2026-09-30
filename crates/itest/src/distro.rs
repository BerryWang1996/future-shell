//! 任意发行版容器的最小助手（M7.1 的对照载体）。
//!
//! 与 [`crate::sshd`] 的分工：那个是**连真 sshd**（协议层要的东西）；这里要的只是
//! 「一台真的 Alpine / Debian / 跑着 systemd 的 Ubuntu，能在里面跑命令」——
//! 系统面工具箱验的是**命令输出的解析**，不是 SSH，绕开 SSH 能省掉一次握手和一套密钥。
//!
//! 两种启动形态：
//! · [`DistroContainer::start`]：普通镜像 + `sleep infinity` 保活，用来验包管理器；
//! · [`DistroContainer::start_systemd`]：`/sbin/init` 作 PID 1 + privileged + host cgroupns，
//!   容器里跑的是**真 systemd**，`systemctl` 才有意义。privileged 是 systemd 要写 cgroup
//!   与挂 tmpfs 的硬要求，不是图省事——它只影响这个一次性测试容器。

use testcontainers::core::CgroupnsMode;
use testcontainers::core::{CmdWaitFor, ExecCommand, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

pub struct DistroContainer {
    container: ContainerAsync<GenericImage>,
    /// 镜像标识，断言失败信息里带上它——同一条断言在不同发行版上失败，原因常常不同。
    pub label: String,
}

/// 一次 `docker exec` 的结果。**不断言退出码**：本模块要跑的命令里有天然非零的
/// （`dnf check-update` 有更新时是 100、`apk version -l` 无匹配时是 1），
/// 断言 0 会把正常情形判成失败。
pub struct ExecOut {
    pub code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
}

impl DistroContainer {
    /// 普通容器：`sleep infinity` 保活。
    pub async fn start(image: &str, tag: &str) -> testcontainers::core::error::Result<Self> {
        let container = GenericImage::new(image, tag)
            .with_wait_for(WaitFor::seconds(1))
            .with_cmd(["sleep", "infinity"])
            .start()
            .await?;
        Ok(Self {
            container,
            label: format!("{image}:{tag}"),
        })
    }

    /// systemd 作 PID 1 的容器。
    ///
    /// `SYSTEMD_IGNORE_CHROOT=1` 让 systemd 不因为「看起来像 chroot」而拒绝启动；
    /// cgroupns=Host + privileged 是它写 `/sys/fs/cgroup` 的前提。等 20 秒够 systemd
    /// 把基本单元拉起来——不等的话 `systemctl list-units` 会在启动早期返回半张表。
    pub async fn start_systemd(
        image: &str,
        tag: &str,
    ) -> testcontainers::core::error::Result<Self> {
        let container = GenericImage::new(image, tag)
            .with_wait_for(WaitFor::seconds(1))
            .with_cmd(["/sbin/init"])
            .with_privileged(true)
            .with_cgroupns_mode(CgroupnsMode::Host)
            .with_env_var("SYSTEMD_IGNORE_CHROOT", "1")
            .start()
            .await?;
        let me = Self {
            container,
            label: format!("{image}:{tag}(systemd)"),
        };
        // 轮询到 systemd 真的接管为止，而不是死等一个固定秒数：慢机器上固定等待会不够，
        // 快机器上又白等。`is-system-running` 在 starting/running/degraded 时都算「已接管」。
        for _ in 0..40 {
            let o = me.exec("systemctl is-system-running 2>&1 || true").await?;
            let s = o.stdout.trim();
            if matches!(s, "running" | "degraded" | "starting" | "maintenance") {
                return Ok(me);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        Ok(me) // 没起来也返回：让测试自己断言并把真实输出打出来，比在助手里 panic 好查
    }

    /// 在容器里跑一段 shell。
    pub async fn exec(&self, script: &str) -> testcontainers::core::error::Result<ExecOut> {
        let mut res = self
            .container
            .exec(
                ExecCommand::new(["sh", "-c", script]).with_cmd_ready_condition(CmdWaitFor::exit()),
            )
            .await?;
        let stdout = String::from_utf8_lossy(&res.stdout_to_vec().await?).to_string();
        let stderr = String::from_utf8_lossy(&res.stderr_to_vec().await?).to_string();
        let code = res.exit_code().await?;
        Ok(ExecOut {
            code,
            stdout,
            stderr,
        })
    }

    /// 跑一段 shell 并断言退出码为 0（用于「准备环境」这类必须成功的步骤）。
    pub async fn exec_ok(&self, script: &str) -> testcontainers::core::error::Result<String> {
        let o = self.exec(script).await?;
        assert_eq!(
            o.code,
            Some(0),
            "[{}] 命令失败：{script}\nstdout: {}\nstderr: {}",
            self.label,
            o.stdout,
            o.stderr
        );
        Ok(o.stdout)
    }
}
