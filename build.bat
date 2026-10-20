@echo off
chcp 65001 >nul
setlocal
cd /d "%~dp0"

echo ============================================
echo   360Folder - 发布构建
echo ============================================
echo.

where cargo >nul 2>&1
if errorlevel 1 (
    echo [错误] 未找到 cargo。
    echo 请先安装 Rust：https://rustup.rs/
    echo 或使用 RustRover 自带的工具链（在 IDE 中直接 Build）。
    pause
    exit /b 1
)

echo [1/2] 编译 release ...
cargo build --release
if errorlevel 1 (
    echo.
    echo [失败] 编译未通过。请查看上方错误信息。
    pause
    exit /b 1
)

echo.
echo [2/2] 完成。产物：
echo   target\release\360Folder.exe
echo.

for %%F in (target\release\360Folder.exe) do echo   大小: %%~zF 字节
echo.
echo 提示：把 360Folder.exe 放到想用的目录即可运行。
echo       首次运行会在同目录生成 Profile\ 保存设置。
echo.
pause
