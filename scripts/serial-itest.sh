#!/usr/bin/env bash
# 串口集成测试（M7.4）：在 Linux 容器里用 socat 造一对伪终端，跑真实的
# 打开 / 收发 / 改波特率 / 拔线重连 / GBK 解码。
#
# ## 为什么必须在容器里跑，而不是在开发机上
#
# 伪终端是 Linux 的东西，`/dev/pts` 在 Windows 上不存在；而容器里的 pty 也不可能被
# 宿主机的 `serialport` 打开（那是容器自己的挂载命名空间）。所以**测试进程本身**得进容器——
# 这一点与 `crates/itest` 的其它用例正相反：那些是宿主机上的测试去驱动容器里的服务。
#
# ## 它证明什么、不证明什么（如实记账）
#
# 证明：`serialport` 走的那条 `open(2)` + `termios` + `read/write` 路径是通的，
#       双向字节严格一致（含 0x00–0xFF 全字节），`tcsetattr` 改波特率成功且端口仍可用，
#       设备消失后会话自动重连且**读流不断**，GBK 字节经整条管道后是 UTF-8。
# 不证明：真实硬件那一层——波特率是否真的按 9600 打在线上、RTS/CTS 电平、USB 转换器
#       拔插时驱动的具体错误码。伪终端没有这些。Windows 的 COM 驱动同理**完全未覆盖**
#       （容器是 Linux）。这两条在路线图里标 [!] 留给用户，不代签。
#
# 用法：bash scripts/serial-itest.sh [--rebuild-image]
set -euo pipefail

IMAGE="fs-serial-itest:cached"

repo_root() {
  cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd
}

build_image() {
  local ctx
  ctx="$(repo_root)/.cache/serial-itest"
  mkdir -p "$ctx"
  cat > "$ctx/Dockerfile" <<'DOCKERFILE'
FROM rust:1.97-bookworm
# socat：造伪终端对。测试进程自己 spawn 它（拔线 = kill 掉，插回 = 再起一个）。
# pkg-config/libudev-dev 刻意**不装**：fs_serial 关掉了 libudev 特性，装了反而掩盖
# 「不装也能编」这个我们要保住的性质。
RUN apt-get update -qq && apt-get install -y -qq socat && rm -rf /var/lib/apt/lists/*
DOCKERFILE
  if command -v cygpath >/dev/null 2>&1; then ctx="$(cygpath -w "$ctx")"; fi
  MSYS_NO_PATHCONV=1 docker build --load -q -t "$IMAGE" "$ctx" >/dev/null
}

main() {
  local root win
  root="$(repo_root)"
  cd "$root"

  if [[ "${1:-}" == "--rebuild-image" ]] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "== 构建串口测试镜像 =="
    build_image
  fi

  if command -v cygpath >/dev/null 2>&1; then win="$(cygpath -w "$root")"; else win="$root"; fi

  # 与 linux-precheck 同款两条硬教训：
  # ① 拷源码时排除 target/（不排除要拷十几 GB）；
  # ② **不能** `cargo test | tail`——管道退出码是 tail 的（恒 0），门禁就无法失败。
  local script
  script="set -e; mkdir -p /w; cd /src;"
  script+=" tar cf - --exclude=target --exclude=node_modules --exclude=./.git --exclude=./.cache --exclude=./.mutation . | (cd /w && tar xf -);"
  script+=" cd /w;"
  script+=" echo '== socat 版本 =='; socat -V | head -1;"
  script+=" echo '== 串口集成测试（真伪终端对）==';"
  script+=" FS_SERIAL_PTY=1 cargo test -p fs_serial --locked --target-dir /tmp/tgt -- --test-threads=1 --nocapture 2>&1 | tail -60;"
  script+=' rc=${PIPESTATUS[0]}; [ "$rc" = "0" ] || exit "$rc"'

  echo "== 在 Linux 容器里跑串口集成测试（挂载：$win）=="
  MSYS_NO_PATHCONV=1 docker run --rm -v "${win}:/src:ro" "$IMAGE" bash -c "$script"

  cat <<'COVERAGE'

== 覆盖清单（如实记账）==
已真实验证（Linux + socat 伪终端对）：
  打开端口 / 双向收发（0x00–0xFF 全字节）/ 改波特率不重开端口 / 拔线后自动重连且读流不断 /
  GBK 字节经整条终端管道后是 UTF-8
未覆盖（路线图里标 [!]，不代签）：
  真实硬件：波特率是否真的打在线上、RTS/CTS 电平、USB 转换器拔插时的驱动错误码
  Windows COM 驱动一路：容器是 Linux，`serialport` 的 Windows 后端在这里一行都没跑到
COVERAGE
}

main "$@"
