@echo off
REM Build/test the bot. Artifacts go to the target directory next to this script.
REM Usage:  build.cmd test --workspace   |   build.cmd build --release   |   build.cmd clippy
REM The Rust toolchain (stable) is taken from PATH. If you keep it outside your user profile,
REM set RUSTUP_HOME and CARGO_HOME first; the script adds %CARGO_HOME%\bin to PATH.
setlocal
if not defined CARGO_TARGET_DIR set "CARGO_TARGET_DIR=%~dp0target"
if defined CARGO_HOME set "PATH=%CARGO_HOME%\bin;%PATH%"
cargo %*
