#requires -Version 7.0
[CmdletBinding(DefaultParameterSetName = 'Project')]
param(
    [Parameter(Mandatory, ParameterSetName = 'Project')][string]$Project,
    [Parameter(Mandatory, ParameterSetName = 'User')][switch]$UserScope,
    [string]$HookExe = (Join-Path $PSScriptRoot '../target/release/mcli-hook.exe'),
    [ValidateSet('both','codex','agy')][string]$Agent = 'both',
    [switch]$Remove
)
$ErrorActionPreference = 'Stop'
$exe = [IO.Path]::GetFullPath($HookExe)
if (!$Remove -and ![IO.File]::Exists($exe)) { throw "找不到 Mcli 钩子程序 $exe" }
if ($exe.IndexOfAny([char[]]'%"') -ge 0) { throw '钩子程序路径不能包含百分号或双引号。' }
if (!$UserScope) { $root = (Get-Item -LiteralPath $Project).FullName }
$codexRoot = if ($UserScope -and $env:CODEX_HOME) { $env:CODEX_HOME }
    elseif ($UserScope) { Join-Path $env:USERPROFILE '.codex' } else { Join-Path $root '.codex' }
$agyRoot = if ($UserScope) { Join-Path $env:USERPROFILE '.gemini/config' } else { Join-Path $root '.agents' }

function Update-Config([string]$provider, [string]$dir, [string[]]$events) {
    $path = Join-Path $dir 'hooks.json'
    $existed = [IO.File]::Exists($path)
    if ($Remove -and !$existed) { return }
    $before = if ($existed) { [IO.File]::ReadAllText($path) }
    $data = if ($existed) { ConvertFrom-Json -AsHashtable -InputObject $before } else { [ordered]@{} }
    if ($data -isnot [Collections.IDictionary]) { throw "配置需要一个 JSON 对象 $path" }
    $section = if ($provider -eq 'codex') { 'hooks' } else { 'mcli-conversations' }
    if (!$data.Contains($section)) {
        if ($Remove) { return }
        $data[$section] = [ordered]@{}
    }
    $group = $data[$section]
    $changed = $false
    foreach ($event in $events) {
        $command = 'cmd.exe /d /v:off /s /c ""{0}" {1} {2}"' -f $exe,$provider,$event
        $existing = if ($group.Contains($event)) { $group[$event] }
        $retained = [Collections.Generic.List[object]]::new()
        $found = $false
        foreach ($entry in $existing) {
            $matchesOwn = if ($provider -eq 'codex') {
                $entry.hooks.Count -eq 1 -and $entry.hooks[0].command -ceq $command
            } else { $entry.command -ceq $command }
            if ($matchesOwn) { $found = $true }
            if (!$Remove -or !$matchesOwn) { $retained.Add($entry) }
        }
        if (!$Remove -and !$found) {
            $hook = [ordered]@{ type = 'command'; command = $command; timeout = 2 }
            if ($provider -eq 'codex') { $retained.Add([ordered]@{hooks = @($hook)}) }
            else { $retained.Add($hook) }
            $changed = $true
        } elseif ($Remove -and $found) { $changed = $true }
        if ($retained.Count) { $group[$event] = $retained.ToArray() }
        else { $group.Remove($event) }
    }
    if (!$changed) { return }
    if (!$group.Count) { $data.Remove($section) }
    [void][IO.Directory]::CreateDirectory($dir)
    $temp = Join-Path $dir ('.mcli-' + [Guid]::NewGuid().ToString('N') + '.tmp')
    try {
        [IO.File]::WriteAllText($temp, (ConvertTo-Json -Depth 100 -InputObject $data) + [Environment]::NewLine)
        if (!$existed) { [IO.File]::Move($temp, $path) }
        else {
            if ([IO.File]::ReadAllText($path) -cne $before) { throw "配置已发生变化，请重试 $path" }
            [IO.File]::Replace($temp, $path, (Join-Path $dir ('.mcli-backup-' + [Guid]::NewGuid().ToString('N') + '.json')))
        }
    } finally { if ([IO.File]::Exists($temp)) { [IO.File]::Delete($temp) } }
    Write-Output $path
}
if ($Agent -ne 'agy') { Update-Config 'codex' $codexRoot @('SessionStart','UserPromptSubmit','PermissionRequest','Stop','Interrupt','SessionEnd') }
if ($Agent -ne 'codex') { Update-Config 'agy' $agyRoot @('SessionStart','PreInvocation','Stop') }
