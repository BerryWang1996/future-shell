# 开发与发布指南

从仓库根目录执行下列命令。产品能力见 [README](README.md)，设计与验收入口见 [文档中心](docs/README.md)。

## 开发环境

| 工具 | 要求 |
|---|---|
| Rust | `rust-toolchain.toml` 固定为 1.97.1 |
| Node.js | 24，前端使用 `npm ci` 安装锁文件中的依赖 |
| Tauri CLI | `cargo install tauri-cli --version "2.11.4" --locked` |
| Docker | Linux 容器集成测试和串口伪终端测试需要 |
| Windows | MSVC Build Tools、Windows SDK、NASM、Git Bash |
| macOS | Xcode Command Line Tools |
| Linux | WebKitGTK 4.1、GTK 3 等开发库，以 CI 中的安装列表为准 |

Windows 使用 Developer Command Prompt，或先加载 MSVC 环境后再启动 Git Bash。
Git 自带的 `link.exe` 可能遮蔽 MSVC 链接器；NASM 必须能从 PATH 找到。

## 构建与运行

```bash
npm --prefix frontend ci
bash scripts/build-rdp-helper.sh --debug
cargo tauri dev
```

仅编译 Rust 或运行 Rust 测试前，还需要 `npm --prefix frontend run build`：
`frontend/dist/` 与 `app/binaries/` 都是 Tauri 编译期前置。

生成安装包：

```bash
bash scripts/build-rdp-helper.sh
cargo tauri build -- --locked
```

产物位于根目录的 `target/release/bundle/`；显式指定 Rust target 时位于对应 target 子目录。
RDP helper 是独立工作区，拥有自己的 Cargo.lock。必须随安装包交付 helper 及三份第三方许可材料。

## 质量检查

`scripts/ci-local.sh` 汇总本机检查，`.github/workflows/ci.yml` 定义三平台门禁。
发布前使用锁定依赖的命令，不能用跳过慢测试的结果代替完整验证。

```bash
cargo fmt --check
cargo fmt --manifest-path rdp-helper/Cargo.toml --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --manifest-path rdp-helper/Cargo.toml --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked
cargo test --workspace --locked
cargo test --manifest-path rdp-helper/Cargo.toml --locked
cargo deny check licenses bans advisories
cargo deny --manifest-path rdp-helper/Cargo.toml check licenses bans advisories
npm --prefix frontend run check
npm --prefix frontend test -- --run
npm --prefix frontend run build
npm --prefix frontend run verify:contrast
npm --prefix frontend audit --omit=dev --registry=https://registry.npmjs.org
node .github/scripts/check-version-consistency.mjs --selftest
node .github/scripts/check-version-consistency.mjs
node scripts/check-doc-links.mjs
```

`nextest` 与 `cargo test` 都要运行：前者隔离每个测试进程，后者可发现进程全局状态在
多线程测试中的冲突。依赖审计通过也不代表没有已知豁免，风险接受记录以 `deny.toml` 为准。

Linux 容器验证：

```bash
bash scripts/linux-itest.sh
bash scripts/serial-itest.sh
```

原生 Linux 也可设置 `FS_ITEST=1` 运行工作区 nextest。Windows/macOS 没有该环境时，
容器测试会经过环境门控，不能将该结果计为真实协议验证。
`FS_PERF_TEST` 是应用的性能观测标记，不是启用容器测试的开关。
GUI 性能自查、自动化吞吐与并发测试的区别见 [性能验证](docs/verification/performance.md)。

## 修改约定

- 提交尽量围绕一个明确问题，提交信息遵循 Conventional Commits。
- 对行为修改提供有意义的回归测试；文档移动须同步修改引用并通过链接检查。
- 不将凭据写入日志；保持零遥测，新的出站连接遵循用户配置边界。
- 依赖许可与漏洞例外以 `deny.toml`、`about.toml` 为准，两个 Rust 工作区都要检查。
- 文本遵循 `.gitattributes` 的 LF 约定；Windows PowerShell 5.1 需要的 UTF-8 BOM 保留。
- 构建缓存、原始测试输出放在 `.cache/`、`target/` 等忽略目录；文档只保留可复核结论与必要证据索引。

## 发布流程

版本号须在主工作区、helper、Tauri、前端与锁文件中一致，数据库版本从 Cargo 包版本派生。
先更新 [CHANGELOG](CHANGELOG.md)，再记录 [发版检查](docs/verification/release-readiness-1.0.0.md)
和 [人工验收](docs/verification/manual-checklist-1.0.0.md)。

`.github/workflows/release.yml` 支持两种入口：

- `workflow_dispatch`：验证打包链路，允许未签名候选包，只上传 artifact。
- `v*.*.*` tag：完整质量门禁、三平台安装包、签名与公证、安装 smoke，通过后创建 draft Release。

Windows 产出 MSI/NSIS，macOS 产出 DMG，Linux 产出 AppImage/deb。
tag 构建强制验证 Windows Authenticode、macOS 签名与公证；所需 secret 名称见发布工作流顶部。

所有 Release 附件统一整理到同一层，包括两个 Rust 工作区和前端 SBOM、三份许可材料。
先生成 SHA-256 校验和，再生成来源证明。审阅 draft、真实安装结果与待验项后才发布。

应用标识固定为 `io.futureshell.app`，影响安装身份及 WebView 存储。已发布后如需更改，必须设计迁移；
应用自己的数据库与日志目录独立于该标识，详见 [便携模式](docs/guides/portable-mode.md)。
