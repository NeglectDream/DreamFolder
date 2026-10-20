# 360Folder 构建脚本（PowerShell 版）
# 用法:  powershell -ExecutionPolicy Bypass -File build.ps1
#        powershell -ExecutionPolicy Bypass -File build.ps1 -Debug
#        powershell -ExecutionPolicy Bypass -File build.ps1 -Run

[CmdletBinding()]
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

Write-Head '360Folder 构建'

# ---- 检查工具链 ----
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    Write-Host '[错误] 未找到 cargo。' -ForegroundColor Red
    Write-Host '  请安装 Rust: https://rustup.rs/'
    Write-Host '  或在 RustRover 中直接使用 IDE 的 Build 功能。'
    exit 1
}
Write-Host "工具链: $((& cargo --version))"

if ($Clean) {
    Write-Host '清理 target/ ...'
    & cargo clean
}

# ---- 编译 ----
$profileArgs = if ($Debug) { @() } else { @('--release') }
$profileName = if ($Debug) { 'debug' } else { 'release' }

Write-Host "开始编译 ($profileName) ..."
& cargo build @profileArgs
if ($LASTEXITCODE -ne 0) {
    Write-Host '[失败] 编译未通过。' -ForegroundColor Red
    exit $LASTEXITCODE
}

# ---- 结果 ----
$exe = Join-Path $PSScriptRoot "target\$profileName\360Folder.exe"
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
    Write-Host '[警告] 未找到产物 exe，请检查 Cargo.toml 的 [[bin]] 配置。' -ForegroundColor Yellow
}
