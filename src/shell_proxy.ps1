# The launcher supplies a private, per-terminal exchange directory. Its files
# contain only current-user DPAPI ciphertext. No proxy values cross the terminal.
function global:__McliSanitizeProxy($url) {
    if ([string]::IsNullOrWhiteSpace($url)) { return '' }
    $clean = ($url.Trim() -replace '[\x00-\x1f\x7f]', '')
    return ($clean -replace '^([A-Za-z0-9+.-]+://)[^/@]+@', '$1')
}

function global:__McliProxySummary {
    $active = $null
    foreach ($name in @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY')) {
        $v = [Environment]::GetEnvironmentVariable($name, 'Process')
        if (![string]::IsNullOrWhiteSpace($v)) {
            $active = __McliSanitizeProxy $v
            break
        }
    }
    if ($active) {
        if ($global:__McliProxyOverrides.Count -eq 0 -and !$global:__McliProxyManual) {
            return 'sys:' + $active
        }
        return $active
    }
    if ($global:__McliProxyDirect -or $global:__McliProxyOverrides.Count -gt 0) {
        return 'off'
    }
    return ''
}

function global:__McliReportProxy([switch]$Force) {
    $sid = [Environment]::GetEnvironmentVariable('MCLI_SESSION_ID', 'Process')
    if ([string]::IsNullOrEmpty($sid)) { return }
    $summary = __McliProxySummary
    if (!$Force -and [object]::Equals($summary, $global:__McliProxyReported)) { return }
    $global:__McliProxyReported = $summary
    [Console]::Write([string][char]27 + ']633;P;McliAgent=' + $sid + '=proxy=' + $summary + [char]7)
}

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
    if (!$global:__McliProxyInternal) {
        $global:__McliProxyManual = $true
        $hasActive = $false
        foreach ($n in @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY')) {
            if (![string]::IsNullOrWhiteSpace($current[$n])) { $hasActive = $true; break }
        }
        if ($hasActive) { $global:__McliProxyDirect = $false }
    }
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
    __McliReportProxy
}

function global:__McliReadProxyCatalog {
    if ([string]::IsNullOrEmpty($global:__McliProxyCatalog) -or ![IO.File]::Exists($global:__McliProxyCatalog)) {
        return $null
    }
    try {
        $raw = [IO.File]::ReadAllText($global:__McliProxyCatalog, [Text.Encoding]::UTF8)
        if ([string]::IsNullOrWhiteSpace($raw)) { return $null }
        return Microsoft.PowerShell.Utility\ConvertFrom-Json -InputObject $raw
    } catch {
        return $null
    }
}

function global:__McliApplyProxyTarget($spec, [bool]$manual) {
    $raw = if ($null -eq $spec) { '' } else { [string]$spec }
    $key = $raw.Trim()
    if (!$key) { return $null }
    $low = $key.ToLowerInvariant()
    $global:__McliProxyInternal = $true
    try {
        if ($low -in @('sys','system','reset')) {
            foreach ($name in $global:__McliProxyNames) {
                $base = $global:__McliProxyBaseline[$name]
                if ($null -eq $base) {
                    [Environment]::SetEnvironmentVariable($name, [NullString]::Value, 'Process')
                } else {
                    [Environment]::SetEnvironmentVariable($name, $base, 'Process')
                }
            }
            $global:__McliProxyDirect = $false
            $global:__McliProxyManual = $false
            __McliCaptureProxy
            __McliReportProxy -Force
            return '系统默认'
        }
        if ($low -in @('off','direct','none','0','__direct__')) {
            foreach ($name in @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY')) {
                [Environment]::SetEnvironmentVariable($name, [NullString]::Value, 'Process')
            }
            $global:__McliProxyDirect = $true
            if ($manual) { $global:__McliProxyManual = $true }
            __McliCaptureProxy
            __McliReportProxy -Force
            return '直连（已清除代理变量）'
        }
        $cat = __McliReadProxyCatalog
        $profiles = @()
        if ($null -ne $cat -and $null -ne $cat.PSObject.Properties['profiles'] -and $null -ne $cat.profiles) {
            $profiles = @($cat.profiles)
        }
        $picked = $null
        $idx = 0
        if ([int]::TryParse($key, [ref]$idx) -and $idx -ge 1 -and $idx -le $profiles.Count) {
            $picked = $profiles[$idx - 1]
        } else {
            foreach ($p in $profiles) {
                if ($null -ne $p -and $null -ne $p.PSObject.Properties['name'] -and ([string]$p.name).Equals($key, [StringComparison]::OrdinalIgnoreCase)) {
                    $picked = $p
                    break
                }
            }
        }
        if ($null -eq $picked) { return $null }
        $url = [string]$picked.url
        foreach ($name in @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY')) {
            [Environment]::SetEnvironmentVariable($name, $url, 'Process')
        }
        $np = if ($null -ne $picked.PSObject.Properties['no_proxy']) { [string]$picked.no_proxy } else { '' }
        if (![string]::IsNullOrWhiteSpace($np)) {
            [Environment]::SetEnvironmentVariable('NO_PROXY', $np.Trim(), 'Process')
        } else {
            $baseNp = $global:__McliProxyBaseline['NO_PROXY']
            if ($null -eq $baseNp) {
                [Environment]::SetEnvironmentVariable('NO_PROXY', [NullString]::Value, 'Process')
            } else {
                [Environment]::SetEnvironmentVariable('NO_PROXY', $baseNp, 'Process')
            }
        }
        $global:__McliProxyDirect = $false
        if ($manual) { $global:__McliProxyManual = $true }
        __McliCaptureProxy
        __McliReportProxy -Force
        return ("$($picked.name) (" + (__McliSanitizeProxy $url) + ")")
    } finally {
        $global:__McliProxyInternal = $false
    }
}

function global:__McliAutoProxyForAgent($agent) {
    if ($global:__McliProxyManual -or [string]::IsNullOrWhiteSpace($agent)) { return }
    $cat = __McliReadProxyCatalog
    if ($null -eq $cat -or $null -eq $cat.PSObject.Properties['agent_defaults'] -or $null -eq $cat.agent_defaults) { return }
    $prop = $cat.agent_defaults.PSObject.Properties[$agent.ToLowerInvariant()]
    if ($null -eq $prop -or [string]::IsNullOrWhiteSpace([string]$prop.Value)) { return }
    $label = __McliApplyProxyTarget ([string]$prop.Value) $false
    if ($label) {
        [Console]::WriteLine("[Mcli] 已为 $agent 自动切换代理：$label")
    }
}

function global:__McliProxyCmd {
    param([string]$Target)
    if ([string]::IsNullOrWhiteSpace($Target)) {
        $summary = __McliProxySummary
        $cur = if (!$summary) { '系统默认（未设置代理）' }
               elseif ($summary -eq 'off') { '直连（已清除代理变量）' }
               elseif ($summary.StartsWith('sys:')) { '系统默认（' + $summary.Substring(4) + '）' }
               else { $summary }
        [Console]::WriteLine("当前终端代理：$cur")
        $cat = __McliReadProxyCatalog
        $profiles = @()
        if ($null -ne $cat -and $null -ne $cat.PSObject.Properties['profiles'] -and $null -ne $cat.profiles) {
            $profiles = @($cat.profiles)
        }
        if ($profiles.Count -eq 0) {
            [Console]::WriteLine('尚未配置代理预设，可在 Mcli 左侧「设置 -> 代理预设」中添加。')
        } else {
            [Console]::WriteLine('可用代理预设：')
            for ($i = 0; $i -lt $profiles.Count; $i++) {
                $p = $profiles[$i]
                [Console]::WriteLine(("  [{0}] {1}  ->  {2}" -f ($i + 1), $p.name, (__McliSanitizeProxy ([string]$p.url))))
            }
        }
        [Console]::WriteLine('用法：mp <预设名|编号> | mp off (直连) | mp sys (恢复系统默认)')
        return
    }
    $applied = __McliApplyProxyTarget $Target $true
    if ($applied) {
        [Console]::WriteLine("[Mcli] 当前终端代理已切换为：$applied")
    } else {
        [Console]::Error.WriteLine("[Mcli] 未找到代理预设「$Target」。输入 mp 查看可用预设，或使用 mp off / mp sys。")
    }
}

# PowerShell defines a built-in ReadOnly alias `mp -> Move-ItemProperty`.
# Replace it with Mcli's proxy switcher so `mp <preset>` works in both pwsh 7 and Windows PowerShell 5.1.
Remove-Item -Path Alias:mp -Force -ErrorAction SilentlyContinue
Set-Alias -Name mp -Value __McliProxyCmd -Scope Global -Force

function global:__McliPinProxy($run) {
    if ($global:__McliProxyFailed) { return }
    __McliCaptureProxy
    [IO.File]::WriteAllText([IO.Path]::Combine($global:__McliProxyDir, $run), $global:__McliProxySeal)
}

& {
    $ErrorActionPreference = 'Stop'
    $global:__McliProxyDir = $env:MCLI_PROXY_DIR
    Remove-Item -LiteralPath Env:MCLI_PROXY_DIR
    $global:__McliProxyCatalog = [Environment]::GetEnvironmentVariable('MCLI_PROXY_CATALOG', 'Process')
    if ($null -ne $global:__McliProxyCatalog) {
        [Environment]::SetEnvironmentVariable('MCLI_PROXY_CATALOG', [NullString]::Value, 'Process')
    }
    $global:__McliProxyNames = @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','NO_PROXY')
    $global:__McliProxyBaseline = @{}
    $global:__McliProxyLast = @{}
    $global:__McliProxyOverrides = @{}
    $global:__McliProxyFailed = $false
    $global:__McliProxyManual = $false
    $global:__McliProxyDirect = $false
    $global:__McliProxyInternal = $false
    $global:__McliProxyReported = $null
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
                $allCleared = $true
                foreach ($p in $saved.PSObject.Properties) {
                    if ($p.Name -notin $global:__McliProxyNames -or
                        ($null -ne $p.Value -and ($p.Value -isnot [string] -or $p.Value.Contains([char]0)))) { throw 'format' }
                    $global:__McliProxyOverrides[$p.Name] = $p.Value
                    if ($p.Name -in @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY') -and ![string]::IsNullOrWhiteSpace($p.Value)) {
                        $allCleared = $false
                    }
                }
                if ($global:__McliProxyOverrides.Count -gt 0) {
                    $global:__McliProxyManual = $true
                    if ($allCleared) { $global:__McliProxyDirect = $true }
                }
                __McliApplyProxy
            } finally { [Array]::Clear($plain, 0, $plain.Length) }
        }
        foreach ($name in $global:__McliProxyNames) {
            $global:__McliProxyLast[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        }
        __McliReportProxy -Force
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
