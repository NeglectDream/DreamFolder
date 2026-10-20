@echo off
rem 一键构建入口。真正的逻辑在 build.ps1 里，这里只做转发，
rem 避免同一套判断在 bat / ps1 两处各维护一份。
chcp 65001 >nul
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build.ps1" %*
