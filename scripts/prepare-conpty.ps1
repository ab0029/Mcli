param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../target/release/conpty'),
    [string]$PackagePath
)

$ErrorActionPreference = 'Stop'
$version = '1.25.261109001'
$packageHash = '3D77F535DF848E44225C2A8912AC844FECB23F3057732B639F3034B587496F09'
$files = @(
    @{ Name = 'conpty.dll'; Entry = 'runtimes/win-x64/native/conpty.dll'; Hash = 'E971856F89748DC3B458125E9AFDD79B5E9574A7C937676116D3CBBD3B71F492' }
    @{ Name = 'OpenConsole.exe'; Entry = 'build/native/runtimes/x64/OpenConsole.exe'; Hash = 'B17E48B07EF153B420F0A6E178BACD31FB4C3390E21AE746586213FC0719A775' }
)
$destination = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
$license = Join-Path $PSScriptRoot '../doc/licenses/Microsoft.Windows.Console.ConPTY.txt'

function Assert-Files([string]$Directory) {
    foreach ($file in $files) {
        $path = Join-Path $Directory $file.Name
        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $file.Hash) {
            throw "文件校验失败：$path"
        }
        $signature = Get-AuthenticodeSignature -LiteralPath $path
        if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch '^CN=Microsoft Corporation,') {
            throw "微软签名校验失败：$path"
        }
    }
    if ((Get-FileHash -LiteralPath (Join-Path $Directory 'LICENSE.txt')).Hash -ne (Get-FileHash -LiteralPath $license).Hash) {
        throw "缺少对应的许可证：$Directory"
    }
}

if (Test-Path -LiteralPath $destination) {
    Assert-Files $destination
    Write-Output "ConPTY $version 已就绪：$destination"
    return
}

$stagingRoot = [IO.Directory]::CreateDirectory((Join-Path $PSScriptRoot '../.tmp/prepare-conpty')).FullName
$staging = [IO.Directory]::CreateDirectory((Join-Path $stagingRoot ([guid]::NewGuid().ToString('N')))).FullName
try {
    if (!$PackagePath) {
        $id = 'microsoft.windows.console.conpty'
        $PackagePath = Join-Path $staging 'package.nupkg'
        Invoke-WebRequest "https://api.nuget.org/v3-flatcontainer/$id/$version/$id.$version.nupkg" -OutFile $PackagePath
    }
    if ((Get-FileHash -LiteralPath $PackagePath -Algorithm SHA256).Hash -ne $packageHash) {
        throw "NuGet 包与固定版本 $version 的校验值不一致"
    }
    $bundle = [IO.Directory]::CreateDirectory((Join-Path $staging 'conpty')).FullName
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead((Get-Item -LiteralPath $PackagePath).FullName)
    try {
        foreach ($file in $files) {
            [IO.Compression.ZipFileExtensions]::ExtractToFile($zip.GetEntry($file.Entry), (Join-Path $bundle $file.Name))
        }
    } finally { $zip.Dispose() }
    Copy-Item -LiteralPath $license -Destination (Join-Path $bundle 'LICENSE.txt')
    Assert-Files $bundle
    [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination))
    try { [IO.Directory]::Move($bundle, $destination) }
    catch [IO.IOException] {
        if (!(Test-Path -LiteralPath $destination)) { throw }
        Assert-Files $destination
    }
    Write-Output "ConPTY $version 已就绪：$destination"
} finally {
    if ([IO.Path]::GetDirectoryName($staging) -eq $stagingRoot) {
        Remove-Item -LiteralPath $staging -Force -Recurse
    }
}
