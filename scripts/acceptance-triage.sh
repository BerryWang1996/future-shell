#!/usr/bin/env bash
# 验收清单的结构性门禁（M4a.1 T95）。
#
# ## 为什么需要它
#
# `docs/verification/phase1-acceptance.md` 当前 39 条未落勾、2 条已落勾。「未落勾」这三个字掩盖了
# 三种完全不同的状态：有测试载体只是没跑过、必须人工做（真机/交互/采购）、以及
# **压根还没有载体**。混在一起的后果是没人知道「还差多少」——离发布的距离不可估。
#
# 本脚本把每条未落勾项归入三桶之一，并断言分类是完整的（每条都必须落桶）。
#
# ## 三条互相独立的做空防护
#
# 对抗审查指出：若只断言「每条都落桶」，把 39 条全塞进「人工」桶即恒绿——门禁被合法
# 做空，黑洞只是换了个位置。故：
#   ① **人工桶条数上限**（MANUAL_MAX）：超了就红，逼新增项优先去找测试载体；
#   ② **被检查条目总数下限**（MIN_ITEMS）：防解析器被改成匹配零条后恒绿；
#   ③ **「人工」的定义收紧**：原文必须含「手工核验/手动核验/真实交互/采购/公证/
#      开发者账号/webdriver」这类字样，且**先判有无载体**——纯软件可自动化的项
#      不许仅因为描述里出现「真机」二字就混进人工桶（实测踩到：L10 的「MITM 抢答与
#      真机竞态」是在描述竞态场景，那一条其实有 `单测 + itest` 双载体）。
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

DOC="docs/verification/phase1-acceptance.md"
# 人工闸门桶的条数上限。**只应随真实的人工项减少而下调，不许为了让门禁变绿而上调。**
MANUAL_MAX=16
# 被检查的未落勾条目下限（防解析器匹配零条）。
MIN_ITEMS=30

fail() { echo "验收分类失败：$*" >&2; exit 1; }
[[ -f "$DOC" ]] || fail "找不到 $DOC"

# 判定顺序要紧：**先看有无载体**。载体是客观的（点名到文件/函数），人工是兜底的；
# 反过来判会把「有测试但描述里提到真机」的条目误塞进人工桶。
CARRIER_RE='载体[ 　]*[=＝]|\.rs\b|\.test\.ts\b|itest|单测|组件测试|E2E'
MANUAL_RE='手工核验|手动核验|真实交互|采购|公证|开发者账号|webdriver|逐屏一致'

carrier=0
manual=0
todo=0
manual_lines=()
todo_lines=()

# 摘要行：按**字符**截断而非字节。`cut -c` 与本机 awk 的 substr 在中文上都会切在
# 多字节序列中间，输出一串乱码（两次实测）。node 的字符串是 UTF-16，切得对。
# 走 **stdin** 而不是 argv：条目原文以 `- ` 起头，作为参数会被 node 当成命令行选项
# （`bad option: - [ ] …`，第三次实测）。
brief() {
  printf '%s' "$1" | node -e '
    let s = "";
    process.stdin.on("data", (c) => (s += c)).on("end", () => {
      const t = s.replace(/^\s*- \[ \] /, "");
      process.stdout.write(t.length > 34 ? t.slice(0, 34) + "…" : t);
    });
  '
}

while IFS= read -r line; do
  # 只吃未落勾项；已落勾的不在本门禁范围内
  [[ "$line" =~ ^[[:space:]]*-[[:space:]]\[[[:space:]]\][[:space:]] ]] || continue
  if grep -qE "$CARRIER_RE" <<<"$line"; then
    carrier=$((carrier + 1))
  elif grep -qE "$MANUAL_RE" <<<"$line"; then
    manual=$((manual + 1))
    manual_lines+=("$(brief "$line")")
  else
    todo=$((todo + 1))
    todo_lines+=("$(brief "$line")")
  fi
done < "$DOC"

total=$((carrier + manual + todo))

# 做空防护②：总数下限
[[ $total -ge $MIN_ITEMS ]] ||
  fail "只解析出 $total 条未落勾项（下限 $MIN_ITEMS）——解析器可能被改坏，这个门禁被做空了"

# 做空防护①：人工桶上限
[[ $manual -le $MANUAL_MAX ]] ||
  fail "人工闸门桶有 $manual 条（上限 $MANUAL_MAX）——新增的未落勾项应优先去建测试载体，\
不要靠塞进人工桶让门禁变绿。若确实都是人工项，请连同理由一并下调/上调本上限。"

cat <<REPORT
验收分类（$DOC，未落勾 $total 条）
  有载体（点名到测试，只差跑一遍）：$carrier
  待人工闸门（真机/交互/采购/公证）：$manual  （上限 $MANUAL_MAX）
  待建载体（既无测试也非人工）    ：$todo
REPORT

if [[ ${#todo_lines[@]} -gt 0 ]]; then
  echo ""
  echo "「待建载体」清单——这才是离发布还差的距离，下一阶段的输入："
  for l in "${todo_lines[@]}"; do echo "  · ${l}"; done
fi
