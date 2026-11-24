# Runs once in this interactive session, after normal PowerShell profiles.
function global:__McliReportDirectory($location) {
    if ($location.Provider.Name -eq 'FileSystem') {
        [Console]::Write([string][char]27 + ']9;9;"' + $location.ProviderPath + '"' + [char]7)
    }
}

& {
    $commands = $ExecutionContext.SessionState.InvokeCommand
    if ($commands.PSObject.Properties['LocationChangedAction']) {
        $notify = [System.EventHandler[System.Management.Automation.LocationChangedEventArgs]] {
            param($sender, $change)
            __McliReportDirectory $change.NewPath
        }
        $commands.LocationChangedAction = [Delegate]::Combine($commands.LocationChangedAction, $notify)
    } else {
        # Windows PowerShell 5.1 exposes its current filesystem directory at the prompt.
        $global:__McliOriginalPrompt = $function:prompt
        function global:prompt {
            $rendered = & $global:__McliOriginalPrompt
            __McliReportDirectory $ExecutionContext.SessionState.Path.CurrentFileSystemLocation
            $rendered
        }
    }
    __McliReportDirectory $ExecutionContext.SessionState.Path.CurrentFileSystemLocation
}

function global:__McliAgentReport($name) {
    [Console]::Write([string][char]27 + ']633;P;McliAgent=' + $env:MCLI_SESSION_ID + '=' + $name + [char]7)
}

# Observe only an accepted, single foreground command that PowerShell subsequently resolves.
# Never rewrite the user's command line or inject extra CLI flags.
& {
    $editor = $ExecutionContext.InvokeCommand.GetCommand('PSConsoleHostReadLine', 'Function')
    if (!$editor -or !$ExecutionContext.InvokeCommand.PSObject.Properties['PostCommandLookupAction']) { return }
    $global:__McliReadLine = $editor.ScriptBlock
    $global:__McliLookup = $null
    $global:__McliAgentPrompt = $null
    function global:__McliLeaveForeground {
        if ($null -eq $global:__McliAgentPrompt) { return }
        $function:global:prompt = $global:__McliAgentPrompt
        $global:__McliAgentPrompt = $null
        $env:MCLI_LAUNCH_ID = $global:__McliPreviousLaunch
        __McliAgentReport shell
    }
    function global:__McliWatchForeground($line) {
        # This helper's scope also supports legacy profiles that enable strict mode.
        Microsoft.PowerShell.Core\Set-StrictMode -Off
        $global:__McliPreparedLine = $line
        $engine = $ExecutionContext.InvokeCommand
        if ($global:__McliLookup) {
            $engine.PostCommandLookupAction = [Delegate]::Remove($engine.PostCommandLookupAction, $global:__McliLookup)
            $global:__McliLookup = $null
        }
        if (!$line) { return }
        $tokens = $errors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseInput($line, [ref]$tokens, [ref]$errors)
        $statements = $ast.EndBlock.Statements
        if ($errors.Count -or $statements.Count -ne 1) { return }
        $pipeline = $statements[0]
        if ($pipeline -isnot [System.Management.Automation.Language.PipelineAst] -or
            $pipeline.Background -or $pipeline.PipelineElements.Count -ne 1) { return }
        $cmd = $pipeline.PipelineElements[0]
        if ($cmd -isnot [System.Management.Automation.Language.CommandAst] -or $cmd.Redirections.Count) { return }
        $name = $cmd.GetCommandName()
        if (!$name) { return }
        $resolved = if ($name -match '(?:^|[\\/])(codex|agy|kimi|grok)(?:\.(exe|cmd|bat|ps1))?$') {
            $engine.GetCommand($name, 'All')
        } else { $engine.GetCommand($name, 'Alias') }
        if ($resolved -is [System.Management.Automation.AliasInfo]) { $resolved = $resolved.ResolvedCommand }
        if (!$resolved -or $resolved.CommandType -notin 'Application','ExternalScript' -or
            $resolved.Name -notmatch '^(codex|agy|kimi|grok)\.(exe|cmd|bat|ps1)$') { return }

        $agentName = $matches[1].ToLowerInvariant()
        $uuidRe = '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
        $safeIdRe = '^[A-Za-z0-9._][A-Za-z0-9._-]{0,127}$'
        $argsList = @()
        for ($i = 1; $i -lt $cmd.CommandElements.Count; $i++) {
            $el = $cmd.CommandElements[$i]
            if ($el -is [System.Management.Automation.Language.StringConstantExpressionAst]) {
                $argsList += $el.Value
            } else {
                $argsList += $el.Extent.Text.Trim("'`"")
            }
        }
        if ($agentName -eq 'kimi') {
            $nonTuiKimi = @('login','doctor','export','fork','migrate','provider','session','upgrade','vis','web','acp','app','import-sessions','install-desktop')
            foreach ($a in $argsList) {
                if ($a -ceq '-p' -or $a -ieq '--prompt' -or $a -like '--prompt=*' -or $a -ceq '-V' -or $a -ieq '--version' -or $a -ceq '-h' -or $a -ieq '--help') { return }
            }
            if ($argsList.Count -gt 0 -and -not $argsList[0].StartsWith('-') -and $argsList[0].ToLowerInvariant() -in $nonTuiKimi) { return }
        } elseif ($agentName -eq 'grok') {
            $nonTuiGrok = @('agent','inspect','doctor','leader','logout','login','mcp','plugin','memory','models','sessions','usage','setup','share','wrap','export','trace','update','version','v','completions','worktree','du','disk-usage','workspace')
            foreach ($a in $argsList) {
                if ($a -ceq '-p' -or $a -ieq '--print' -or $a -ceq '-V' -or $a -ieq '--version' -or $a -ceq '-h' -or $a -ieq '--help') { return }
            }
            if ($argsList.Count -gt 0 -and -not $argsList[0].StartsWith('-') -and $argsList[0].ToLowerInvariant() -in $nonTuiGrok) { return }
        }

        $global:__McliAgentName = $agentName
        $global:__McliRun = [Guid]::NewGuid().ToString('N')
        $global:__McliAgentPath = $resolved.Path
        $explicitId = $null
        if ($agentName -eq 'codex') {
            for ($i = 0; $i -lt ($argsList.Count - 1); $i++) {
                if ($argsList[$i] -ieq 'resume' -and $argsList[$i + 1] -match $uuidRe) {
                    $explicitId = $argsList[$i + 1].ToLowerInvariant()
                    break
                }
            }
        } elseif ($agentName -eq 'agy') {
            for ($i = 0; $i -lt $argsList.Count; $i++) {
                $arg = $argsList[$i]
                if ($arg -match '^(?i)--(?:conversation|resume)=(.+)$') {
                    $candidate = $matches[1]
                    if ($candidate -match $uuidRe) {
                        $explicitId = $candidate.ToLowerInvariant()
                        break
                    }
                }
                if (($arg -ieq '--conversation' -or $arg -ieq '--resume') -and ($i + 1) -lt $argsList.Count -and $argsList[$i + 1] -match $uuidRe) {
                    $explicitId = $argsList[$i + 1].ToLowerInvariant()
                    break
                }
            }
        } elseif ($agentName -eq 'kimi') {
            for ($i = 0; $i -lt $argsList.Count; $i++) {
                $arg = $argsList[$i]
                if ($arg -match '^(?i)--(?:session|resume)=(.+)$') {
                    $candidate = $matches[1]
                    if ($candidate -match $safeIdRe) {
                        $explicitId = $candidate
                        break
                    }
                }
                if (($arg -ieq '--session' -or $arg -ceq '-S' -or $arg -ieq '--resume' -or $arg -ceq '-r') -and ($i + 1) -lt $argsList.Count -and $argsList[$i + 1] -match $safeIdRe) {
                    $explicitId = $argsList[$i + 1]
                    break
                }
            }
        } elseif ($agentName -eq 'grok') {
            for ($i = 0; $i -lt $argsList.Count; $i++) {
                $arg = $argsList[$i]
                if ($arg -match '^(?i)--resume=(.+)$') {
                    $candidate = $matches[1]
                    if ($candidate -match $safeIdRe) {
                        $explicitId = $candidate
                        break
                    }
                }
                if (($arg -ieq '--resume' -or $arg -ceq '-r') -and ($i + 1) -lt $argsList.Count -and $argsList[$i + 1] -match $safeIdRe) {
                    $explicitId = $argsList[$i + 1]
                    break
                }
            }
        }
        $global:__McliExplicitId = $explicitId
        $global:__McliLookup = [EventHandler[System.Management.Automation.CommandLookupEventArgs]] {
            param($sender, $lookup)
            $actual = $lookup.Command
            if ($actual -is [System.Management.Automation.AliasInfo]) { $actual = $actual.ResolvedCommand }
            if (!$actual -or $actual.CommandType -notin 'Application','ExternalScript' -or
                $actual.Path -ne $global:__McliAgentPath) { return }
            $context = $ExecutionContext.InvokeCommand
            $context.PostCommandLookupAction = [Delegate]::Remove($context.PostCommandLookupAction, $global:__McliLookup)
            $global:__McliLookup = $null
            $global:__McliAgentPrompt = $function:prompt
            $global:__McliPreviousLaunch = $env:MCLI_LAUNCH_ID
            $run = $global:__McliRun
            if ($context.GetCommand('__McliPinProxy', 'Function')) {
                try { __McliPinProxy $run } catch { [Console]::Error.WriteLine('未能为本次对话保存代理设置。') }
            }
            $env:MCLI_LAUNCH_ID = $run
            function global:prompt {
                $rendered = & $global:__McliAgentPrompt
                __McliLeaveForeground
                $rendered
            }
            $report = $global:__McliAgentName + '=' + $run
            if ($global:__McliExplicitId) { $report += '=' + $global:__McliExplicitId }
            __McliAgentReport $report
        }
        $engine.PostCommandLookupAction = [Delegate]::Combine($engine.PostCommandLookupAction, $global:__McliLookup)
    }
    function global:PSConsoleHostReadLine {
        # The original editor reads $? immediately to show the previous command's result.
        $line = & $global:__McliReadLine
        try { __McliWatchForeground $line } catch { Write-Debug $_ }
        $global:__McliPreparedLine
    }
}
