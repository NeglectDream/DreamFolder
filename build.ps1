# DreamFolder 构建脚本（PowerShell 版）
# 用法:  powershell -ExecutionPolicy Bypass -File build.ps1
#        powershell -ExecutionPolicy Bypass -File build.ps1 -Debug
#        powershell -ExecutionPolicy Bypass -File build.ps1 -Run
#
# 工具链由项目配置决定，脚本不再重复判断：
#   - rust-toolchain.toml  -> 固定使用 MinGW-GNU 工具链（本机无 MSVC 链接器）
# 构建产物落在项目内的 target\ 下。详见 README「五、构建」。

# 注意：这里刻意不加 [CmdletBinding()]——它会自动暴露通用参数 -Debug，
# 与下面自定义的 $Debug 开关冲突（报 "对命令多次定义了名为 Debug 的参数"）。
param(
    [switch]$Debug,      # 构建 debug 版（编译更快，运行更慢）
    [switch]$Run,        # 构建后立即运行
    [switch]$Clean       # 先清理再构建
)

$ErrorActionPreference = 'Stop'
Set-Location -Path $PSScriptRoot

function Write-Head($text) {
    Write-Host ''
    Write-Host '============================================' -ForegroundColor Cyan
    Write-Host "  $text" -ForegroundColor Cyan
    Write-Host '============================================' -ForegroundColor Cyan
}

Write-Head 'DreamFolder 构建'

# ---- 找 cargo：rustup 装完后 %USERPROFILE%\.cargo\bin 不一定在当前 PATH 里 ----
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $candidates = @((Join-Path $env:USERPROFILE '.cargo\bin'))
    if ($env:CARGO_HOME) { $candidates += (Join-Path $env:CARGO_HOME 'bin') }
    foreach ($dir in $candidates) {
        if (Test-Path (Join-Path $dir 'cargo.exe')) {
            $env:PATH = "$dir;$env:PATH"
            break
        }
    }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host '[错误] 未找到 cargo。' -ForegroundColor Red
    Write-Host '  请安装 Rust: https://rustup.rs/'
    Write-Host '  或在 RustRover 中直接使用 IDE 的 Build 功能。'
    exit 1
}
Write-Host "工具链: $((& cargo --version))"
# 让 cargo 报出它实际选的 host，便于确认 rust-toolchain.toml 生效
$hostLine = (& cargo -vV) -match '^host:'
if ($hostLine) { Write-Host "  $hostLine" }

if ($Clean) {
    Write-Host '清理构建目录 ...'
    & cargo clean
}

# ---- 编译 ----
# 参数直接写，不用数组 splatting——& 调用下 @profileArgs 会被拆成
# `-` 与 `release` 两个参数，cargo 报 "unexpected argument '-'"。
$profileName = if ($Debug) { 'debug' } else { 'release' }

Write-Host "开始编译 ($profileName) ..."
if ($Debug) {
    & cargo build
} else {
    & cargo build --release
}
if ($LASTEXITCODE -ne 0) {
    Write-Host '[失败] 编译未通过。' -ForegroundColor Red
    exit $LASTEXITCODE
}

# ---- 报告产物 ----
$targetRoot = Join-Path $PSScriptRoot 'target'
$exe = Join-Path $targetRoot "$profileName\DreamFolder.exe"
if (Test-Path $exe) {
    $size = (Get-Item $exe).Length
    Write-Host ''
    Write-Host '构建成功' -ForegroundColor Green
    Write-Host "  产物: $exe"
    Write-Host ("  大小: {0:N0} 字节 ({1:N2} MiB)" -f $size, ($size / 1MB))

    if ($Run) {
        Write-Host '启动 ...'
        & $exe
    }
} else {
    Write-Host "[警告] 在 $exe 未找到产物，请检查 Cargo.toml 的 [[bin]] 配置。" -ForegroundColor Yellow
}
