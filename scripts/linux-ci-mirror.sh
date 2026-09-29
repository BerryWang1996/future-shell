#!/usr/bin/env bash
# ubuntu CI `check` 作业的本地镜像（1.0.0 候选首次 GitHub CI 失败后补）。
#
# ## 为什么需要它
#
# `linux-precheck.sh` 只编 4 个 crate（不含 app——app 要 Tauri Linux 全栈）。1.0.0 候选
# 的第一次 GitHub CI 在 ubuntu 上死于 `crates/serial/tests/pty.rs` 的 clippy
# （`#![cfg(unix)]` 文件，Windows 上根本不编译），而 fs_serial 不在那 4 个里。
# 更要紧的是 clippy 在第一处错误就停了——后面还有没有 Linux 专属的红，推上去之前
# 只能靠这个脚本回答。
#
# 本脚本在容器里按 `.github/workflows/ci.yml` 的 check 作业**原顺序**跑：
# 前端 dist → helper → fmt ×2 → clippy ×2 → helper test → nextest → cargo test。
#
# ## 不覆盖（如实记账）
#
# - `FS_ITEST=1` 的容器 itest：要 Docker socket 直通，走 `scripts/linux-itest.sh`。
# - `cargo deny`：与平台无关，本机 ci-local 已跑。
# - 工具链：仓库 `rust-toolchain.toml` 钉 1.97.1，CI 的 rustup 同样会被它钉住，两边一致。
#
# ## 磁盘（先看余量再跑）
#
# target 放在具名卷 `fs-linux-ci-target`（增量复用），2026-09-29 实测 **22 GB**。
# Docker Desktop（WSL2）的数据盘是只涨不缩的 vhdx：`docker volume rm fs-linux-ci-target`
# 只把空间还给 Docker 自己复用，**Windows 侧的可用空间不会回来**（fstrim 只回收了几百 MB）。
# 同一天本机 C 盘因此与 Windows 侧的 target/ 一起被写满到 19 MB。跑之前留出 30 GB 以上。
#
# 用法：bash scripts/linux-ci-mirror.sh [--rebuild-image]
# 可选：APT_MIRROR=mirrors.tuna.tsinghua.edu.cn（deb.debian.org 慢时构建镜像用；
#       webkit 一组依赖约 400 MB，默认源在本机实测 ~300 kB/s）
set -euo pipefail

IMAGE="fs-linux-ci:cached"
VOLUME="fs-linux-ci-target"

repo_root() { cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd; }

build_image() {
  local ctx
  ctx="$(repo_root)/.cache/linux-ci"
  mkdir -p "$ctx"
  cat > "$ctx/Dockerfile" <<'DOCKERFILE'
FROM rust:1.97-bookworm
ARG APT_MIRROR=deb.debian.org
RUN sed -i "s#deb.debian.org#${APT_MIRROR}#g" /etc/apt/sources.list.d/debian.sources
# 与 ci.yml「Install Linux webkit deps」同一组包；nasm/cmake/clang 是 -sys crate 前置，
# openssh-client 给 ssh_agent 的真实签名往返，socat 给串口伪终端测试（设 FS_SERIAL_PTY 时）。
RUN apt-get update -qq \
 && apt-get install -y -qq libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
      librsvg2-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libdbus-1-dev libxdo-dev libssl-dev \
      nasm cmake clang pkg-config openssh-client socat xz-utils \
 && rm -rf /var/lib/apt/lists/*
# Node 24（与 CI 的 setup-node 同一主版本）
RUN curl -fsSL https://nodejs.org/dist/v24.16.0/node-v24.16.0-linux-x64.tar.xz \
    | tar -xJ -C /usr/local --strip-components=1
RUN rustup component add rustfmt clippy \
 && curl -LsSf https://get.nexte.st/latest/linux | tar zxf - -C /usr/local/cargo/bin
DOCKERFILE
  if command -v cygpath >/dev/null 2>&1; then ctx="$(cygpath -w "$ctx")"; fi
  MSYS_NO_PATHCONV=1 docker build --load --progress=plain     --build-arg "APT_MIRROR=${APT_MIRROR:-deb.debian.org}" -t "$IMAGE" "$ctx"
}

main() {
  local root win
  root="$(repo_root)"
  cd "$root"
  if [[ "${1:-}" == "--rebuild-image" ]] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "== 构建 CI 镜像镜像 =="
    build_image
  fi
  if command -v cygpath >/dev/null 2>&1; then win="$(cygpath -w "$root")"; else win="$root"; fi

  # 每步独立判退出码（不经管道，避免「| tail 吞退出码」那一类假绿，见 linux-precheck.sh 教训 ②）
  local script='set -euo pipefail
mkdir -p /w && cd /src
tar cf - --exclude=./target --exclude=./rdp-helper/target --exclude=node_modules \
  --exclude=./.git --exclude=./.cache --exclude=./frontend/dist . | (cd /w && tar xf -)
cd /w
# target 目录用符号链接指进具名卷，而不是设 CARGO_TARGET_DIR：build-rdp-helper.sh 与
# rdp itest 都按 rdp-helper/target/<profile>/ 找 helper，改环境变量会让它们找错地方。
mkdir -p /tgt/main /tgt/helper
ln -s /tgt/main /w/target
ln -s /tgt/helper /w/rdp-helper/target
step() { echo; echo "════ $* ════"; "$@"; }
step bash -c "cd frontend && npm ci --no-audit --no-fund >/dev/null && npm run build >/dev/null"
step bash scripts/build-rdp-helper.sh --debug
step cargo fmt --check
step cargo fmt --manifest-path rdp-helper/Cargo.toml --check
step cargo clippy --workspace --all-targets --locked -- -D warnings
step cargo clippy --manifest-path rdp-helper/Cargo.toml --all-targets --locked -- -D warnings
step cargo test --manifest-path rdp-helper/Cargo.toml --locked
step cargo nextest run --workspace --locked --no-tests pass
step cargo test --workspace --locked
echo; echo "════ ubuntu check 作业镜像：全部通过 ════"'
  MSYS_NO_PATHCONV=1 docker run --rm -v "${win}:/src:ro" -v "${VOLUME}:/tgt" "$IMAGE" bash -c "$script"
}

main "$@"
