#!/usr/bin/env bash
# M1 出口回归门禁（M4b 出口第 19 项「全量 M1 出口标准回归不劣化」）。
#
# ## 「不劣化」要怎么证
#
# 「跑一遍全量测试，全绿」证不了这件事。全量绿只说明**现在还剩下的那些**测试都过了——
# 如果 M4b 期间某个 M1 的测试文件被删掉、或者某条断言被改松，全量照样绿。
#
# 而那正是这条出口真正要防的：M1 的判据在后续里程碑里被悄悄拆掉。
#
# 所以本脚本做两件全量跑不做的事：
#
#   ① **载体存在性**：路线图 M1 出口每条判据里点名的测试文件必须还在。
#      删掉一个，M1 那一条就失去了判据，而路线图上的 `[x]` 还挂着。
#   ② **载体清单从路线图现解析**，不写死在脚本里。写死等于双份维护，
#      而两份迟早分叉——分叉之后这个脚本守的就不是路线图上那份清单了。
#
# 跑测试本身仍然交给 `ci-local.sh`（含 `--with-itest` 时的容器一路）：
# 那些文件不是孤立可跑的，它们混在各自的 crate / vitest 套件里。
# 本脚本的定位是**清单核对**，不是第二个测试运行器。
#
# 用法：bash scripts/m1-regression.sh
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

ROADMAP="docs/roadmap.md"
# 载体数下限。**只应随真实的载体增加而上调，不许为了让门禁变绿而下调。**
# 解析器一旦被改坏（正则失配、段落定位失效）会抓到零条，那时空集恒满足「都存在」。
MIN_CARRIERS=25

fail=0
note() { printf '%s\n' "$*"; }
bad() { printf '  ✗ %s\n' "$*"; fail=1; }

note "== M1 出口回归门禁 =="
note ""

[ -f "$ROADMAP" ] || { note "找不到 $ROADMAP"; exit 1; }

# ── ① 解析 M1 出口段 ────────────────────────────────────────────────────────
#
# 段落边界用两个**内容锚点**而不是行号：行号会随文档编辑漂，而漂了之后
# 解析出的是别的里程碑的载体，门禁看着还在跑、守的却是另一件事。
readarray -t CARRIERS < <(
  awk '
    /本清单为 Phase 1 唯一验收权威/ { inseg = 1 }
    /^- \*\*tag\*\*：`v0\.1\.0`/    { inseg = 0 }
    inseg { print }
  ' "$ROADMAP" |
  grep -oE '`[A-Za-z0-9_./-]+\.(rs|test\.ts|ts|mjs)`' |
  tr -d '`' |
  sort -u
)

COUNT=${#CARRIERS[@]}
note "路线图 M1 出口点名的测试载体：$COUNT 个"

if [ "$COUNT" -lt "$MIN_CARRIERS" ]; then
  bad "只解析到 $COUNT 个载体（下限 ${MIN_CARRIERS}）——多半是段落锚点或正则失效了。"
  bad "  空集会让下面的存在性检查恒真，那时这个门禁什么也没守。"
  exit 1
fi

# ── ② 存在性核对 ───────────────────────────────────────────────────────────
#
# 判据里写的多是**文件名**（`term.test.ts`）而非完整路径，故按文件名全仓找。
#
# `git ls-files` 只用来**列候选路径**，存在与否一律由 `[ -f ]` 判——前者查的是
# git 索引，删掉工作区文件而不 `git rm` 时它照样报「在」。而「文件被删了但还没
# 提交」恰恰是这道闸最该拦住的时刻：那正是有人正在拆掉一个 M1 判据的那一刻。
note ""
note "载体存在性："
missing=0
for c in "${CARRIERS[@]}"; do
  base="$(basename "$c")"
  found=0
  while IFS= read -r p; do
    [ -f "$p" ] && { found=1; break; }
  done < <(git ls-files "*$base")
  if [ "$found" -eq 0 ]; then
    bad "$c —— 载体已不在工作区，而路线图上那一条还挂着 [x]"
    missing=$((missing + 1))
  fi
done
[ "$missing" -eq 0 ] && note "  全部 $COUNT 个载体都在"

# ── ③ 落勾状态核对 ─────────────────────────────────────────────────────────
#
# 顺带把 M1 出口的落勾分布打出来。这不是断言（`[~]`/`[!]` 是合法状态，
# 它们各自写了未闭合的理由），而是**让「还差多少」在每次跑的时候都被看见**——
# 一个只在文档里的数字，读的人比跑的人少得多。
note ""
SEG="$(awk '/本清单为 Phase 1 唯一验收权威/{i=1} /^- \*\*tag\*\*：`v0\.1\.0`/{i=0} i' "$ROADMAP")"
done_n="$(printf '%s\n' "$SEG" | grep -cE '^\s*- \[x\]' || true)"
part_n="$(printf '%s\n' "$SEG" | grep -cE '^\s*- \[~\]' || true)"
block_n="$(printf '%s\n' "$SEG" | grep -cE '^\s*- \[!\]' || true)"
todo_n="$(printf '%s\n' "$SEG" | grep -cE '^\s*- \[ \]' || true)"
note "M1 出口落勾分布：已闭合 $done_n / 部分 $part_n / 阻塞 $block_n / 未开始 $todo_n"

# 「未开始」是唯一不该出现的状态：M1 早已交付，一条 `[ ]` 意味着有条目从没被处置过。
if [ "$todo_n" -gt 0 ]; then
  bad "M1 出口里还有 $todo_n 条 [ ]（从未处置）——M1 已交付，这个状态不该存在"
fi

note ""
if [ "$fail" -eq 0 ]; then
  note "M1 出口载体齐备。测试是否全绿由 scripts/ci-local.sh 负责（本脚本只核对清单）。"
else
  note "M1 出口回归门禁失败。"
fi
exit "$fail"
