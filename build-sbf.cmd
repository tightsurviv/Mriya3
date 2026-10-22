@echo off
REM Builds the on-chain program to SBF (.so). Requires the Solana CLI (Anza v4.3.0) with cargo-build-sbf.
REM Usage:  build-sbf.cmd --manifest-path crates\program-sbf\Cargo.toml
REM (or cd into the program package directory and run:  ..\..\build-sbf.cmd)
REM Output: target-sbf\deploy\arb_program_sbf.so
REM DEPLOYING to mainnet/Agave: add  --arch v3  (mainnet requires SBPF v3; the default v0 build is
REM rejected at deploy). The default build is only for solana-program-test unit tests.
REM Optional: SOLANA_BIN = directory with cargo-build-sbf (when it is not in PATH),
REM RUSTUP_HOME / CARGO_HOME as in build.cmd.
setlocal
if not defined CARGO_TARGET_DIR set "CARGO_TARGET_DIR=%~dp0target-sbf"
if defined SOLANA_BIN set "PATH=%SOLANA_BIN%;%PATH%"
if defined CARGO_HOME set "PATH=%CARGO_HOME%\bin;%PATH%"
cargo-build-sbf %*
