
# App-requested restores run inside the original interactive PowerShell. The CLI
# inherits the console directly on every attempt, including stderr and keyboard input.
function __McliRestore($agent, $command) {
    Microsoft.PowerShell.Core\Set-StrictMode -Off
    $outcome = 'resume-ended'
    $claimed = $cleanExit = $null
    try {
        $proxy = $ExecutionContext.InvokeCommand.GetCommand('__McliApplyProxy', 'Function')
        if ($proxy -and $global:__McliProxyFailed) { $outcome = 'resume-unavailable'; return }
        if (!$ExecutionContext.InvokeCommand.GetCommand($agent, 'All')) {
            [Console]::WriteLine("找不到 $agent，请检查该 CLI 的安装位置或 PATH。")
            $outcome = 'resume-unavailable'
            return
        }
        $eventName = 'Local\' + $env:MCLI_SESSION_ID + '-restore-'
        $claimed = [Threading.EventWaitHandle]::new($false, 'ManualReset', ($eventName + 'claimed'))
        $cleanExit = [Threading.EventWaitHandle]::new($false, 'ManualReset', ($eventName + 'clean-exit'))
        __McliAgentReport resume-start
        $watch = $ExecutionContext.InvokeCommand.GetCommand('__McliWatchForeground', 'Function')
        $delays = @(1, 2, 4)
        $directory = $ExecutionContext.SessionState.Path.CurrentFileSystemLocation.ProviderPath
        for ($attempt = 0; $attempt -le $delays.Count; $attempt++) {
            if ($attempt -gt 0) {
                if ($claimed.WaitOne(0)) { return }
                try {
                    Microsoft.PowerShell.Management\Set-Location -LiteralPath $directory -ErrorAction Stop
                } catch {
                    [Console]::Error.WriteLine("无法进入恢复目录 $directory。$($_.Exception.Message)")
                    $outcome = 'resume-unavailable'
                    return
                }
            }
            $null = $cleanExit.Reset()
            if ($proxy) { __McliApplyProxy }
            $line = $command
            if ($watch) {
                try {
                    __McliWatchForeground $command
                    $line = $global:__McliPreparedLine
                } catch { Write-Debug $_ }
            }
            # Read the native exit code here even for profiles which turn it into a PS error.
            $PSNativeCommandUseErrorActionPreference = $false
            $global:LASTEXITCODE = 0
            try {
                & ([ScriptBlock]::Create($line))
                $ok = $?
                $code = $global:LASTEXITCODE
            } catch [System.Management.Automation.PipelineStoppedException] {
                throw
            } catch {
                [Console]::Error.WriteLine($_.Exception.Message)
                $ok = $false
                $code = 1
            } finally {
                if ($watch) { __McliLeaveForeground }
            }
            if ($claimed.WaitOne(0) -or $code -in 130, -1073741510, 3221225786) { return }
            if ($code -eq 0 -and $ok -and $cleanExit.WaitOne(0)) { return }
            if ($attempt -eq $delays.Count) {
                [Console]::WriteLine("会话恢复失败，已重试 $attempt 次。可从历史会话再次恢复。")
                $outcome = 'resume-exhausted'
                return
            }
            $seconds = $delays[$attempt]
            [Console]::WriteLine("会话恢复中断，$seconds 秒后重试（$($attempt + 1)/$($delays.Count)）。按 Ctrl+C 取消。")
            if ($claimed.WaitOne($seconds * 1000)) {
                [Console]::WriteLine('已停止自动恢复。')
                return
            }
            # The history worker can confirm a clean exit during this backoff.
            if ($code -eq 0 -and $ok -and $cleanExit.WaitOne(0)) { return }
        }
    } finally {
        __McliAgentReport $outcome
        if ($cleanExit) { $cleanExit.Dispose() }
        if ($claimed) { $claimed.Dispose() }
    }
}
