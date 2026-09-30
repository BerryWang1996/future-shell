#!/usr/bin/env bash
# Linux 跨平台预检（M4a.1 T88）。
#
# ## 为什么需要它
#
# 本仓的 `#[cfg(unix)]` / `#[cfg(target_os = ...)]` 代码在 Windows 上**根本不编译**：
# 开发机上写错了也永远是绿的。规划期第一次把这份代码放进 Linux 容器，当场撞出两个缺陷
# （T87）：`connmgr/db.rs` 的 `tracing::error!` 格式串写错（编译失败）、`sshengine/transfer.rs`
# 多余的 `OpenOptionsExt` 导入（`clippy -D warnings` 判死）。两处都躺了很久。
#
# CI 本该抓到，但 CI 从未对这份代码跑过（origin 停在 Initial commit）。在能推送之前，
# 这个脚本就是 Linux 一路的唯一门禁。
#
# ## 覆盖与不覆盖（如实记账，不含糊）
#
# 覆盖：`fs_vault` / `fs_connmgr` / `fs_terminal` / `fs_sshengine` —— 生产 `cfg` 站点 16/26。
# 不覆盖：`app` crate 的 10 处。app 要编译需 Tauri Linux 全栈（libwebkit2gtk-4.1-dev 等 9 个
#         系统包）+ Node 24 + 预构建的 `frontend/dist`（`tauri::generate_context!` 的编译期前置）。
#         那是一次镜像工程，本阶段不做——写在这里，免得「16/26」被读成「全都编过了」。
# 也不覆盖：`fs_itest`（要 Docker-in-Docker）、测试代码里的 6 处 cfg 站点（不进产物）。
#
# ## 两个实测教训（都写进了实现，别退回去）
#
# ① **必须排除 `target/`**：不排除时 `tar` 要拷 17 GB，容器十分钟都起不来。
# ② **绝不能写 `cargo check ... | tail`**：管道退出码是 `tail` 的（恒 0），门禁于是
#    **无法失败**。变异验证第一版就栽在这上面——两条真缺陷全部假活。用 PIPESTATUS。
#
# 用法：bash scripts/linux-precheck.sh [--rebuild-image]
set -euo pipefail

IMAGE="fs-linux-precheck:cached"
CRATES=(-p fs_vault -p fs_connmgr -p fs_terminal -p fs_sshengine)

repo_root() {
  cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd
}

# 容器里 `bash -c` 用的一步：跑命令、只留尾部输出，但用 PIPESTATUS 取**左侧**退出码。
# 这一行是本脚本能否失败的全部关键，改动前先读上面的教训 ②。
step() {
  printf '%s 2>&1 | tail -30; rc=${PIPESTATUS[0]}; [ "$rc" = "0" ] || exit "$rc"' "$1"
}

build_image() {
  local ctx
  ctx="$(repo_root)/.cache/precheck"
  mkdir -p "$ctx"
  cat > "$ctx/Dockerfile" <<'DOCKERFILE'
FROM rust:1.97-bookworm
# nasm：aws-lc-sys 的 build script 前置（russh 默认特性）
# cmake/clang/pkg-config/libssl-dev：若干 -sys crate 的构建前置
# openssh-client：ssh_agent.rs 的「真实签名往返」要真的 ssh-agent/ssh-add。
#   假 agent 只回身份列表、不签名，而签名恰是 agent 认证真正的那一步。
#   这个包不在的话那条测试会 panic（不是跳过）——悄悄跳过等于让它变成恒绿的装饰。
RUN apt-get update -qq \
 && apt-get install -y -qq nasm cmake clang pkg-config libssl-dev openssh-client \
 && rm -rf /var/lib/apt/lists/*
DOCKERFILE
  # 构建上下文要给 **Windows 路径**：`MSYS_NO_PATHCONV=1` 关掉了 MSYS 的自动转换，
  # 于是 docker 会原样收到 `/c/Users/...` 并报 "path not found"。
  # 这一处曾经是坏的（镜像已存在时 build_image 根本不会被调用，所以没人撞见），
  # 直到 2026-08-26 加 openssh-client 需要 --rebuild-image 才暴露。
  if command -v cygpath >/dev/null 2>&1; then
    ctx="$(cygpath -w "$ctx")"
  fi
  # --load：Docker Desktop 的 buildx 默认不把结果加载进本地镜像库，不加这个后面 run 会说找不到
  MSYS_NO_PATHCONV=1 docker build --load -q -t "$IMAGE" "$ctx" >/dev/null
}

main() {
  local root win
  root="$(repo_root)"
  cd "$root"

  if [[ "${1:-}" == "--rebuild-image" ]] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "== 构建预检镜像 =="
    build_image
  fi

  # Windows 侧路径：docker 认不得 /c/... 形式的 MSYS 路径
  if command -v cygpath >/dev/null 2>&1; then
    win="$(cygpath -w "$root")"
  else
    win="$root"
  fi

  local script
  script="set -e; mkdir -p /w; cd /src;"
  # 只拷源码：排除 target/（17 GB 的教训）、node_modules、.git
  script+=" tar cf - --exclude=target --exclude=node_modules --exclude=./.git --exclude=./.cache --exclude=./.mutation . | (cd /w && tar xf -);"
  script+=" cd /w;"
  script+=" echo '== cargo check（Linux）==';"
  script+=" $(step "cargo check ${CRATES[*]} --target-dir /tmp/tgt");"
  script+=" echo '== cargo clippy -D warnings（Linux）==';"
  script+=" $(step "cargo clippy ${CRATES[*]} --all-targets --target-dir /tmp/tgt -- -D warnings");"
  script+=" echo '== 单元测试（Linux）==';"
  script+=" $(step "cargo test ${CRATES[*]} --target-dir /tmp/tgt")"

  echo "== 在 Linux 容器里预检（挂载：${win}）=="
  MSYS_NO_PATHCONV=1 docker run --rm -v "${win}:/src:ro" "$IMAGE" bash -c "$script"

  cat <<'COVERAGE'

== 覆盖清单（如实记账）==
已在 Linux 上真实编译并 clippy/测试通过：
  fs_vault(8) fs_connmgr(5) fs_sshengine(2) fs_terminal(1) —— 生产 cfg 站点 16/26
未覆盖：
  app(10)     —— 需 Tauri Linux 全栈 + Node + 预构建 frontend/dist，本阶段不做
  测试代码 6 处 cfg 站点 —— 不进产物，不计入平台风险
本脚本不跑但**另有去处**：
  fs_itest    —— 容器集成测试，见 scripts/linux-itest.sh
                 （Docker socket 直通 + --network host；单列一个脚本是因为挂 socket
                  等于把守护进程的控制权交给容器里的代码，该是一次看得见的选择）
COVERAGE
}

main "$@"
