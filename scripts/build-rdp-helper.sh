#!/usr/bin/env bash
# 构建 RDP helper 并按 Tauri `externalBin` 的命名规矩放进 app/binaries/。
#
# ## 为什么需要这一步
#
# helper 是**独立工作区**（见 rdp-helper/Cargo.toml 顶部：IronRDP 与 russh 的
# 依赖树不能相遇），主工作区的 `cargo build` 不会顺手编它。而它必须与主程序
# 一起装到用户机器上——没有它，用户点 RDP 只会得到一句「安装不完整」。
#
# ## 命名规矩
#
# Tauri 的 externalBin 要求文件名带**目标三元组**后缀
# （`fs-rdp-helper-x86_64-pc-windows-msvc.exe`），打包时会去掉后缀放到
# 主程序 exe 同目录——正是 `app/src/rdp.rs::helper_path()` 找的位置。
#
# ## 用法
#
#   bash scripts/build-rdp-helper.sh                        # 宿主三元组（本地）
#   bash scripts/build-rdp-helper.sh --debug                # debug（本地跑 itest 用）
#   bash scripts/build-rdp-helper.sh --target <triple>      # 交叉/多目标（CI 用）
#
# `cargo tauri build` 之前必须先跑一次这个脚本（CI 里由 release.yml 调用；
# macOS 通用二进制要对每个 rust target 各跑一次 --target）。
set -euo pipefail

cd "$(dirname "$0")/.."
PROFILE="release"
CARGO_FLAG="--release"
TARGET=""

while [ $# -gt 0 ]; do
  case "$1" in
    --debug)  PROFILE="debug"; CARGO_FLAG="" ;;
    --target) shift; TARGET="${1:?--target 需要一个三元组参数}" ;;
    *) echo "未知参数：$1" >&2; exit 2 ;;
  esac
  shift
done

# 目标三元组：显式 --target 优先；否则取 rustc 宿主（不写死：换机器必须跟着变）
if [ -n "$TARGET" ]; then
  TRIPLE="$TARGET"
  TARGET_FLAG="--target $TARGET"
else
  TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
  TARGET_FLAG=""
fi
EXT=""
case "$TRIPLE" in
  *windows*) EXT=".exe" ;;
esac

echo "── 构建 RDP helper（${PROFILE}，${TRIPLE}）──"
# shellcheck disable=SC2086  # TARGET_FLAG 按词拆分是有意的
cargo build --manifest-path rdp-helper/Cargo.toml --locked $CARGO_FLAG $TARGET_FLAG

if [ -n "$TARGET" ]; then
  SRC="rdp-helper/target/$TRIPLE/$PROFILE/fs-rdp-helper$EXT"
else
  SRC="rdp-helper/target/$PROFILE/fs-rdp-helper$EXT"
fi
DEST_DIR="app/binaries"
DEST="$DEST_DIR/fs-rdp-helper-$TRIPLE$EXT"

if [ ! -f "$SRC" ]; then
  echo "构建产物不在预期位置：$SRC" >&2
  exit 1
fi

mkdir -p "$DEST_DIR"
cp "$SRC" "$DEST"
echo "已放置：$DEST"
