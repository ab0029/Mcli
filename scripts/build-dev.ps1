$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$devDir = Join-Path $repoRoot 'target\dev'
$devExe = Join-Path $devDir 'mcli.exe'
$previewDir = Join-Path $repoRoot 'target\dev-preview'
$releaseConpty = Join-Path $repoRoot 'target\release\conpty'
$devConpty = Join-Path $devDir 'conpty'

# Never interfere with target\release\mcli.exe; only check if an existing dev instance locks target\dev\mcli.exe.
$runningDev = Get-Process mcli -ErrorAction SilentlyContinue | Where-Object {
    $_.Path -and ($_.Path -ieq $devExe)
}
if ($runningDev) {
    throw "开发版 $devExe 正在运行（PID: $($runningDev.Id -join ', ')），请先关闭开发版窗口后再重新构建。"
}

. (Join-Path $PSScriptRoot 'use-rust.ps1')

Push-Location $repoRoot
try {
    cargo build --profile dev-preview --locked --offline
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    New-Item -ItemType Directory -Force -Path $devDir | Out-Null
    Copy-Item -LiteralPath (Join-Path $previewDir 'mcli.exe') -Destination $devExe -Force
    $hookSrc = Join-Path $previewDir 'mcli-hook.exe'
    if (Test-Path -LiteralPath $hookSrc) {
        Copy-Item -LiteralPath $hookSrc -Destination (Join-Path $devDir 'mcli-hook.exe') -Force
    }

    if (!(Test-Path -LiteralPath $devConpty) -and (Test-Path -LiteralPath $releaseConpty)) {
        Copy-Item -LiteralPath $releaseConpty -Destination $devConpty -Recurse -Force
    }
    & (Join-Path $PSScriptRoot 'prepare-conpty.ps1') -OutputDirectory $devConpty

    $info = Get-Item -LiteralPath $devExe
    Write-Output "开发版已就绪：$($info.FullName)（$($info.Length) 字节，数据目录：%LOCALAPPDATA%\Mcli-dev）"
} finally {
    Pop-Location
}
