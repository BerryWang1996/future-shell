#!/usr/bin/env bash
# 保留已有 CI/本地入口；解析与自检共用 Node 实现，覆盖两个工作区和锁文件。
set -euo pipefail
exec node "$(dirname "${BASH_SOURCE[0]}")/check-version-consistency.mjs" "$@"
