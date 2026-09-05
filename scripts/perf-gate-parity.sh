#!/usr/bin/env bash
# perf-gate 文档与代码的防漂门禁（M4a.1 T91）。
#
# ## 为什么需要它
#
# Task 64 把冷启动自查线从 P50≤300/P95≤500 校准到 800/1200，**改了 perf.rs 没改
# perf-gate.md**。于是文档虚报门禁 2.6×：读文档的人以为超过 300ms 就会红，实际到 800ms
# 才红。这类漂移没有任何东西会发现——两份文件谁也不认识谁。
#
# 本脚本断言：perf-gate.md「有载体的自查线」表里每一行的 `测试函数` 与 `阈值常量`，
# 都能在 crates/itest/tests/perf.rs 的**同名函数体内**找到那个常量。
#
# ## 做空防护（三条互相独立）
#
# ① 行数下限：表格解析出的行数 < MIN_ROWS 即红（防解析器被改成匹配零行后恒绿）；
# ② 函数必须真实存在于 perf.rs（防表格写一个不存在的函数名蒙混）；
# ③ 常量必须出现在**该函数体内**，不是全文件里随便哪儿（防拿别的函数的数字凑数）。
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

DOC="docs/verification/performance.md"
SRC="crates/itest/tests/perf.rs"
# 有数值阈值的行数下限。加自查线时同步上调；删自查线会在这里绊一下。
MIN_ROWS=4

fail() { echo "perf-gate 对齐失败：$*" >&2; exit 1; }

[[ -f "$DOC" ]] || fail "找不到 $DOC"
[[ -f "$SRC" ]] || fail "找不到 $SRC"

# 取 perf.rs 里 `fn NAME` 到下一个 `fn ` 之间的函数体
fn_body() {
  local name="$1"
  awk -v target="$name" '
    /^[[:space:]]*(async[[:space:]]+)?fn[[:space:]]+[A-Za-z0-9_]+/ {
      # 进入新函数：先判断上一个是否是目标
      match($0, /fn[[:space:]]+[A-Za-z0-9_]+/)
      cur = substr($0, RSTART+3, RLENGTH-3)
      gsub(/[[:space:]]/, "", cur)
      inside = (cur == target)
    }
    inside { print }
  ' "$SRC"
}

rows=0
checked=0
while IFS= read -r line; do
  # 只吃「| 不变量 | 自查线 | `函数` | `常量` |」这种四列行；跳过表头与分隔行
  [[ "$line" == \|*\| ]] || continue
  [[ "$line" == *"测试函数"* ]] && continue
  [[ "$line" == *"---"* ]] && continue

  # 按列切，不要用「找一个反引号词」——那会抓到最后一列的常量当函数名（第一版的 bug）。
  # 列：1 空 | 2 不变量 | 3 自查线 | 4 测试函数 | 5 阈值常量 | 6 空
  IFS='|' read -r -a cols <<<"$line"
  [[ ${#cols[@]} -ge 5 ]] || continue
  fn="$(tr -d ' `' <<<"${cols[3]}")"
  [[ -n "$fn" ]] || continue
  rows=$((rows + 1))

  # 第五列：可能是「`800.0` / `1200.0`」这种多常量，也可能是「（无数值阈值）」
  mapfile -t consts < <(grep -oE '`[0-9]+(\.[0-9]+)?`' <<<"${cols[4]}" | tr -d '`')

  # 防空：表格写了函数名就必须在 perf.rs 里真实存在
  body="$(fn_body "$fn")"
  [[ -n "$body" ]] || fail "表格里的测试函数 \`$fn\` 在 $SRC 里不存在"

  for c in "${consts[@]:-}"; do
    [[ -n "$c" ]] || continue
    # 常量必须出现在**该函数体内**的 assert 里，而不是全文件随便哪儿
    grep -qE "assert!\(.*<=[[:space:]]*${c}\b" <<<"$body" ||
      fail "\`$fn\` 的阈值常量 $c 在 $SRC 的该函数体里找不到对应 assert（文档与代码漂了）"
    checked=$((checked + 1))
  done
done < "$DOC"

[[ $rows -ge $MIN_ROWS ]] ||
  fail "只解析出 $rows 行自查线（下限 $MIN_ROWS）——解析器可能被改坏，这个检查被做空了"
[[ $checked -ge $MIN_ROWS ]] ||
  fail "只核对了 $checked 个阈值常量（下限 $MIN_ROWS）——表格里的常量列可能被清空了"

echo "perf-gate 对齐：$rows 行自查线、$checked 个阈值常量与 $SRC 逐一对上（下限 $MIN_ROWS）"
