#!/usr/bin/env bash
# 在 Linux 容器里跑 fs_itest 的容器集成测试。
#
# ## 与 linux-precheck.sh 的分工
#
# precheck 跑的是**编译面**（cargo check / clippy / 单元测试），刻意不含 fs_itest——
# 它的「覆盖清单」里写着「fs_itest —— 需 Docker-in-Docker」。本脚本就是那一句的解法。
#
# 分成两个脚本而不是加个 flag：本脚本要把宿主的 **Docker socket 挂进容器**，
# 那等于把 Docker 守护进程的完全控制权交给容器里的代码。这是一次实打实的
# 权限扩大，应当是一次显式的、看得见的选择，而不是藏在另一个脚本的某个分支里。
#
# ## 为什么这样就能跑起来
#
# 两件事同时成立才行，本机（Docker Desktop + WSL2）都实测过：
#   ① `-v //var/run/docker.sock:/var/run/docker.sock`——容器里的 testcontainers
#      直接指挥宿主的守护进程，起出来的 sshd 容器是宿主的**兄弟**容器，不是嵌套的。
#      （真正的 dind 要跑一个特权守护进程，慢且脆。）
#   ② `--network host`——testcontainers 的 `get_host()` 给的是 `localhost`，
#      而兄弟容器发布的端口在 WSL2 VM 的 localhost 上。不加这一条，容器里的
#      localhost 是它自己，连不到任何东西。
#
# `MSYS_NO_PATHCONV=1` 是 Git Bash 的必需品：不加它，`//var/run/...` 会被 MSYS
# 改写成 `C:\Program Files\Git\var\run\...`，docker 报 "Access is denied"（实测）。
set -euo pipefail

# 与 linux-precheck.sh 共用同一个镜像（那边的 IMAGE 常量，改名要同步改这里）。
# 不自己建一份：两份 Dockerfile 会各自漂移，而「agent 测试需要 openssh-client」
# 这类前置只写在其中一份里的时候，另一份跑出来的红完全指不到真因。
IMAGE="fs-linux-precheck:cached"

repo_root() { git rev-parse --show-toplevel; }

main() {
  local root win filter
  root="$(repo_root)"
  cd "$root"
  # 可选：只跑匹配的测试（如 `linux-itest.sh agent`）。留空则跑全部。
  filter="${1:-}"

  if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "预检镜像不存在，先跑 scripts/linux-precheck.sh 构建它" >&2
    exit 1
  fi
  # openssh-client 是 ssh_agent.rs 真实签名往返的硬前置。镜像可能是加它之前建的，
  # 那时那条测试会以「起不了 ssh-agent」panic——这里提前查，报错指向真因。
  if ! MSYS_NO_PATHCONV=1 docker run --rm "$IMAGE" sh -c 'command -v ssh-agent >/dev/null'; then
    echo "镜像里没有 ssh-agent：跑 scripts/linux-precheck.sh --rebuild-image 重建" >&2
    exit 1
  fi

  if command -v cygpath >/dev/null 2>&1; then
    win="$(cygpath -w "$root")"
  else
    win="$root"
  fi

  local script
  script="set -e; mkdir -p /w; cd /src;"
  script+=" tar cf - --exclude=target --exclude=node_modules --exclude=./.git --exclude=./.cache --exclude=./.mutation . | (cd /w && tar xf -);"
  script+=" cd /w;"
  script+=" echo '== fs_itest（Linux，容器集成）=='"
  # --test-threads=1：每条测试都起一个 sshd 容器，并行会同时拉起一堆，
  # 在 CI 机器上把内存吃光。慢是可接受的，OOM 之后那种「随机某条红」不可接受。
  script+="; FS_ITEST=1 cargo test -p fs_itest --locked --no-fail-fast --target-dir /tmp/tgt ${filter} -- --test-threads=1"

  echo "== 在 Linux 容器里跑 fs_itest（挂载源码：${win}；Docker socket 直通）=="
  MSYS_NO_PATHCONV=1 docker run --rm \
    --network host \
    -v "${win}:/src:ro" \
    -v //var/run/docker.sock:/var/run/docker.sock \
    "$IMAGE" bash -c "$script"
}

main "$@"
