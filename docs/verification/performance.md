# 性能验证

性能验证分为 Linux 自动化负载测试与本机 GUI 自查。旧性能报告合并在
[历史记录](../archive/README.md)，不代表 1.0.0 的性能结论。

## CI 中的负载测试

设置 `FS_ITEST=1` 后，下列测试在 Linux 容器中实际执行：

- `crates/itest/tests/scale.rs`：千级目录、多文件往返、64 MiB 双向传输及合计吞吐 ≥20 MB/s。
- `crates/itest/tests/render_pipeline.rs`：200 MB 数据零丢失、渲染队列水位、
  本地连接到可交互 <2s、100 并发会话后端内存预算和 PTY 全屏 TUI。

运行 `bash scripts/linux-itest.sh`；本次运行结果见 [1.0.0 发版检查](release-readiness-1.0.0.md)。
这些结果覆盖协议与后端管道，不能替代 WebView 实际绘制、远程网络环境和真机操作的测量。

## 本机 GUI 自查

`crates/itest/tests/perf.rs` 全部使用 `#[ignore]`，普通 CI 不执行。
需要已构建的 release 程序与 GUI 会话，运行时会启动应用：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\perf-gate.ps1
```

脚本返回 cargo 的退出码，报告输出到 `.cache/performance/perf-gate-<时间戳>.md`。
保留原始报告，在发版记录中引用环境、版本、结果和测量边界；失败应先复核原因。
代码中使用 `FS_PERF_TEST` 发出就绪标记，它不表示真实 UI 已可交互。

## 有载体的自查线（perf.rs）

下表由 `scripts/perf-gate-parity.sh` 检查函数与阈值是否一致。

| 不变量 | 自查线 | 测试函数 | 阈值常量 |
|---|---|---|---|
| 冷启动 | P50 / P95（进程内就绪观测） | `coldstart_benchmark` | `800.0` / `1200.0` |
| 冷启动 | P50（coldstart.ps1） | `coldstart_under_1500ms` | `1500` |
| 资源占用 | 主进程空闲 RSS | `memory_under_120mb` | `120` |
| 资源占用 | 主进程空闲 CPU P95 | `cpu_p95_under_5percent` | `5.0` |
| 启动存活 | 进程存活并可强制回收 | `acceptance_local` | （无数值阈值） |

冷启动两项只在 Windows 编译。RSS/CPU 不包含 WebView 子进程，
强制回收也不是优雅退出测试。通过这些自查不等于整机性能达到产品目标。
