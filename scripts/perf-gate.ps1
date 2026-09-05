# perf-gate.ps1 — 发布性能门禁（审计 2026-08-14 挂账项「性能门禁自动化」的落地）
# 用法：powershell -ExecutionPolicy Bypass -File scripts\perf-gate.ps1
#
# 做三件事：
# 1. 自举 MSVC/NASM 环境（Git Bash 的 link.exe 遮蔽 MSVC 链接器；NASM 是
#    aws-lc-sys 的硬前置——见构建环境记录）
# 2. 跑 perf.rs 全部 #[ignore] 自查线（--ignored --test-threads=1 --nocapture）
# 3. 把完整输出 + 主机信息 + git 提交写入 .cache/performance/<时间戳>.md
#
# 退出码 = cargo 的退出码（非零即有自查线被击穿）。
#
# 定位：本机 GUI 自查，记录原始输出供复核。Linux CI 的吞吐/并发/数据流测试
# 是独立证据，不由本脚本代替；范围见 docs/verification/performance.md。
#
# 注：本文件须保持 UTF-8 **带 BOM**——Windows PowerShell 5.1 对无 BOM 的 UTF-8
# 按 ANSI（GBK）解析，中文注释会变成字符串终止符错误。

$ErrorActionPreference = 'Stop'
$Root = Split-Path $PSScriptRoot -Parent

# ── 1. MSVC/NASM 自举 ─────────────────────────────────────────────────────────
$vcvars = @(
    "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat",
    "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat",
    "$env:ProgramFiles\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat",
    "$env:ProgramFiles\Microsoft Visual Studio\2022\Professional\VC\Auxiliary\Build\vcvars64.bat",
    "$env:ProgramFiles\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvars64.bat"
) | Where-Object { Test-Path $_ } | Select-Object -First 1

$nasm = Join-Path $env:USERPROFILE '.local\nasm\nasm-3.02'
if (Test-Path $nasm) { $env:PATH = "$nasm;$env:PATH" }

# 中文工作区路径经 cmd 的代码页会损坏——用 8.3 短路径绕开（构建环境记录的既有手法）。
$fso = New-Object -ComObject Scripting.FileSystemObject
$shortRoot = $fso.GetFolder($Root).ShortPath

$cargoArgs = 'cargo test -p fs_itest --test perf --release --locked -- --ignored --nocapture --test-threads=1'
# chcp 65001：cargo/测试的 UTF-8 中文输出经默认 GBK 代码页解码会变乱码，
# 首跑报告（perf-gate-20260820-214103）里 ✓ 与中文全部损坏即此因。
$cmd = "cd /d `"$shortRoot`" && chcp 65001 >nul && $cargoArgs"
if ($vcvars) { $cmd = "`"$vcvars`" >nul 2>&1 && $cmd" }

# PowerShell 侧按 UTF-8 解码子进程输出（与 chcp 配套）。
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)

# ── 2. 跑门禁 ─────────────────────────────────────────────────────────────────
Write-Host "== 性能门禁：$cargoArgs"
if ($vcvars) { Write-Host "== vcvars: $vcvars" } else { Write-Host "== vcvars: 未找到，沿用调用方环境" }
# cargo 的进度行走 stderr；PS5.1 在 EAP=Stop 下会把原生命令的 stderr 当异常
# 终止脚本。捕获期间放宽到 Continue（异常行为只对本段解除，其余段落仍严格）。
$prevEap = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
$output = cmd /c $cmd 2>&1
$exit = $LASTEXITCODE
$ErrorActionPreference = $prevEap

# ── 3. 写报告 ─────────────────────────────────────────────────────────────────
$reportDir = Join-Path $Root '.cache\performance'
New-Item -ItemType Directory -Force -Path $reportDir | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$report = Join-Path $reportDir "perf-gate-$stamp.md"

$gitRev = git -C $Root rev-parse HEAD 2>$null
if (-not $gitRev) { $gitRev = '(unknown)' }
$gitTag = git -C $Root describe --tags 2>$null
if (-not $gitTag) { $gitTag = '(untagged)' }
$os = (Get-CimInstance Win32_OperatingSystem).Caption
$cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name

if ($exit -eq 0) { $resultText = '通过' } else { $resultText = "失败（exit $exit）" }
$summary = @($output | Where-Object { $_ -match '^test\s+\S+\s+\.\.\.' })

$body = New-Object System.Collections.Generic.List[string]
$body.Add("# 性能门禁报告 $stamp")
$body.Add('')
$body.Add("- 日期：$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')")
$body.Add("- git：$gitRev（$gitTag）")
$body.Add("- 主机：$os / $cpu")
$body.Add("- 命令：$cargoArgs")
$body.Add("- 结果：**$resultText**")
$body.Add('')
$body.Add('## 测试结果摘要')
$body.Add('')
$body.Add('```text')
foreach ($s in $summary) { $body.Add("$s") }
$body.Add('```')
$body.Add('')
$body.Add('## 完整输出')
$body.Add('')
$body.Add('```text')
foreach ($line in $output) { $body.Add("$line") }
$body.Add('```')
$body.Add('')
$body.Add('## 覆盖边界（不因本报告存在而改变）')
$body.Add('')
$body.Add('本报告只覆盖 perf.rs 的 GUI 自查（就绪标记、主进程内存/CPU、进程存活）。')
$body.Add('Linux CI 的并发会话、吞吐及数据流测试独立执行；本报告不能替代其运行结果。')
$body.Add('测量范围和限制见 docs/verification/performance.md。')
$body.Add('')

[System.IO.File]::WriteAllLines($report, $body, (New-Object System.Text.UTF8Encoding($false)))

Write-Host ''
Write-Host "== 报告已写入：$report"
exit $exit
