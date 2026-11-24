# The launcher supplies a private, per-terminal exchange directory. Its files
# contain only current-user DPAPI ciphertext. No proxy values cross the terminal.
function global:__McliApplyProxy {
    foreach ($name in $global:__McliProxyOverrides.Keys) {
        $value = $global:__McliProxyOverrides[$name]
        if ($null -eq $value) {
            [Environment]::SetEnvironmentVariable($name, [NullString]::Value, 'Process')
        } else {
            [Environment]::SetEnvironmentVariable($name, $value, 'Process')
        }
    }
}

function global:__McliCaptureProxy {
    if ($global:__McliProxyFailed) { return }
    $ErrorActionPreference = 'Stop'
    $current = @{}
    $delta = $global:__McliProxyOverrides.Clone()
    $changed = $false
    foreach ($name in $global:__McliProxyNames) {
        $value = [Environment]::GetEnvironmentVariable($name, 'Process')
        $current[$name] = $value
        if ([object]::Equals($value, $global:__McliProxyLast[$name])) { continue }
        $changed = $true
        if ([object]::Equals($value, $global:__McliProxyBaseline[$name])) {
            $delta.Remove($name)
        } else {
            $delta[$name] = $value
        }
    }
    if (!$changed) { return }
    $seal = ''
    if ($delta.Count) {
        $json = Microsoft.PowerShell.Utility\ConvertTo-Json -InputObject $delta -Compress
        $plain = [Text.Encoding]::UTF8.GetBytes($json)
        try {
            $blob = [Security.Cryptography.ProtectedData]::Protect($plain, $null, 'CurrentUser')
            $seal = [Convert]::ToBase64String($blob)
        } finally { [Array]::Clear($plain, 0, $plain.Length) }
    }
    $pending = [IO.Path]::Combine($global:__McliProxyDir, 'pending')
    [IO.File]::WriteAllText($pending, $seal)
    [IO.File]::Replace($pending, $global:__McliProxyFile, [NullString]::Value)
    $global:__McliProxySeal = $seal
    $global:__McliProxyLast = $current
    $global:__McliProxyOverrides = $delta
}

function global:__McliPinProxy($run) {
    if ($global:__McliProxyFailed) { return }
    __McliCaptureProxy
    [IO.File]::WriteAllText([IO.Path]::Combine($global:__McliProxyDir, $run), $global:__McliProxySeal)
}

& {
    $ErrorActionPreference = 'Stop'
    $global:__McliProxyDir = $env:MCLI_PROXY_DIR
    Remove-Item -LiteralPath Env:MCLI_PROXY_DIR
    $global:__McliProxyNames = @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','NO_PROXY')
    $global:__McliProxyBaseline = @{}
    $global:__McliProxyLast = @{}
    $global:__McliProxyOverrides = @{}
    $global:__McliProxyFailed = $false
    foreach ($name in $global:__McliProxyNames) {
        $global:__McliProxyBaseline[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
    }
    try {
        if ($PSVersionTable.PSVersion.Major -le 5) { Add-Type -AssemblyName System.Security }
        $global:__McliProxyFile = [IO.Path]::Combine($global:__McliProxyDir, 'current')
        $global:__McliProxySeal = [IO.File]::ReadAllText($global:__McliProxyFile)
        if ($global:__McliProxySeal) {
            $blob = [Convert]::FromBase64String($global:__McliProxySeal)
            $plain = [Security.Cryptography.ProtectedData]::Unprotect($blob, $null, 'CurrentUser')
            try {
                $saved = Microsoft.PowerShell.Utility\ConvertFrom-Json -InputObject ([Text.Encoding]::UTF8.GetString($plain))
                if ($saved -isnot [pscustomobject]) { throw 'format' }
                foreach ($p in $saved.PSObject.Properties) {
                    if ($p.Name -notin $global:__McliProxyNames -or
                        ($null -ne $p.Value -and ($p.Value -isnot [string] -or $p.Value.Contains([char]0)))) { throw 'format' }
                    $global:__McliProxyOverrides[$p.Name] = $p.Value
                }
                __McliApplyProxy
            } finally { [Array]::Clear($plain, 0, $plain.Length) }
        }
        foreach ($name in $global:__McliProxyNames) {
            $global:__McliProxyLast[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        }
    } catch {
        $global:__McliProxyFailed = $true
        [Console]::Error.WriteLine('无法读取此终端保存的代理设置，已停止自动恢复。')
    }
    $global:__McliProxyPrompt = $function:prompt
    function global:prompt {
        $rendered = & $global:__McliProxyPrompt
        try { __McliCaptureProxy } catch { [Console]::Error.WriteLine('未能保存此终端的代理设置。') }
        $rendered
    }
}
