#requires -Version 7.0
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$toolBin = Join-Path $projectRoot '.tools/rust/bin'
$localCargo = Join-Path $toolBin 'cargo.exe'
$localRustc = Join-Path $toolBin 'rustc.exe'

if ((Test-Path -LiteralPath $localCargo) -and (Test-Path -LiteralPath $localRustc)) {
    if (($env:PATH -split ';')[0] -ne $toolBin) {
        $env:PATH = "$toolBin;$env:PATH"
    }
    $env:CARGO_HOME = Join-Path $projectRoot '.tools/cargo'
}

if (-not (Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue) -or
    -not (Get-Command rustc -CommandType Application -ErrorAction SilentlyContinue)) {
    throw '未找到 Rust 工具链。请按 README 中的 Windows 开发环境说明安装 Rust。'
}
