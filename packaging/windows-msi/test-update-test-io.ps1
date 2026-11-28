<#
  Exercises UpdateTestIo.ps1 and the file-reading functions of
  msi-update-scenarios.ps1 against a fake app in other processes: a file
  held open without sharing, replaced by rename, deleted, and a JSON-lines
  log appended in two writes. Runs on any pwsh 7 (Linux too: .NET enforces
  FileShare between processes there as well). Prints PASS/FAIL; exit 1 on
  any failure. Used by packaging/test_windows_msi_update.py.
#>
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'UpdateTestIo.ps1')
# The scenario script's own functions, without running it.
$ast = [System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'msi-update-scenarios.ps1'), [ref] $null, [ref] $null)
foreach ($name in 'Wait-Until', 'Test-InstallerBusy', 'Get-MsiexecClients', 'Wait-InstallerIdle', 'Get-UiStarts', 'Wait-UiStart', 'Get-JournalPath', 'Get-Journal', 'Get-LastUpdate', 'Wait-LastUpdate', 'Get-FilesVersion') {
    $f = $ast.Find({ param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name }, $true)
    if (-not $f) { throw "msi-update-scenarios.ps1 has no $name" }
    . ([scriptblock]::Create($f.Extent.Text))
}

$failed = 0
function Check([string] $What, [bool] $Ok, [string] $Detail = '') {
    if ($Ok) { Write-Host "PASS  $What" } else { Write-Host "FAIL  $What $Detail"; $script:failed++ }
}
$work = Join-Path ([IO.Path]::GetTempPath()) ('update-test-io-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
$pwsh = (Get-Process -Id $PID).Path

# Run $Code in another pwsh process; returns once it wrote $Ready (if given).
function Start-Fake([string] $Code, [string] $Ready) {
    $p = Start-Process -FilePath $pwsh -PassThru -ArgumentList '-NoProfile', '-NonInteractive', '-EncodedCommand',
        ([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($Code)))
    $null = $p.Handle
    if ($Ready) {
        $deadline = (Get-Date).AddSeconds(30)
        while (-not (Test-Path -LiteralPath $Ready)) {
            if ((Get-Date) -gt $deadline -or $p.HasExited) { throw "fake app did not start: $Code" }
            Start-Sleep -Milliseconds 20
        }
    }
    return $p
}

try {
    # Absent: a missing file and a missing folder.
    Check 'missing file is $null' ($null -eq (Read-SharedJson (Join-Path $work 'none.json')))
    Check 'missing folder is $null' ($null -eq (Read-SharedJson (Join-Path $work 'no\such\dir.json')))
    Check 'missing journal is $null' ($null -eq (Get-Journal (Join-Path $work 'nohome')))
    Check 'missing status is $null' ($null -eq (Get-LastUpdate (Join-Path $work 'nohome')))

    # Held open without sharing (as during the app's rename): waited for, never absent.
    $held = Join-Path $work 'held.json'
    [IO.File]::WriteAllText($held, '{"phase":"installing"}')
    $p = Start-Fake "`$s = [IO.File]::Open('$held', 'Open', 'ReadWrite', 'None'); 'x' | Set-Content '$held.ready'; Start-Sleep -Milliseconds 1500; `$s.Dispose()" "$held.ready"
    $t0 = Get-Date
    $j = Read-SharedJson $held
    $ms = ((Get-Date) - $t0).TotalMilliseconds
    Check 'a held file is read once released' ((Get-Prop $j 'phase') -eq 'installing' -and $ms -ge 500) "after $ms ms"
    $p.WaitForExit()
    $p = Start-Fake "`$s = [IO.File]::Open('$held', 'Open', 'ReadWrite', 'None'); 'x' | Set-Content '$held.ready2'; Start-Sleep -Seconds 5; `$s.Dispose()" "$held.ready2"
    $threw = $false
    try { $null = Read-SharedJson $held 1 } catch { $threw = $_.Exception.InnerException -is [IO.IOException] -or $_.Exception -is [IO.IOException] }
    Check 'a file held past the deadline throws (never $null)' $threw
    $p.WaitForExit()

    # A reader never blocks the app: the fake app renames over and deletes the
    # file while this process reads it with Read-SharedBytes.
    $home1 = Join-Path $work 'home1'
    New-Item -ItemType Directory -Path (Join-Path $home1 'update') | Out-Null
    $jp = Get-JournalPath $home1
    $p = Start-Fake @"
`$ErrorActionPreference = 'Stop'
`$jp = '$jp'; `$errors = 0
'x' | Set-Content '$jp.ready'
for (`$i = 0; `$i -lt 3000; `$i++) {
    try {
        [IO.File]::WriteAllText("`$jp.tmp", '{"phase":"installing","n":' + `$i + '}')
        [IO.File]::Move("`$jp.tmp", `$jp, `$true)
        if (`$i % 7 -eq 0) { [IO.File]::Delete(`$jp) }
    } catch { `$errors++ }
}
[IO.File]::WriteAllText('$jp.errors', [string] `$errors)
"@ "$jp.ready"
    $reads = 0; $absent = 0; $bad = 0; $err = ''
    while (-not $p.HasExited) {
        try {
            $j = Get-Journal $home1
            if ($null -eq $j) { $absent++ } elseif ((Get-Prop $j 'phase') -ne 'installing') { $bad++ }
            $reads++
        } catch { $err = $_.Exception.Message; break }
    }
    $p.WaitForExit()
    $appErrors = [int] (Get-Content -LiteralPath "$jp.errors" -Raw)
    Check 'replace/delete race: every read is whole or absent' ($err -eq '' -and $bad -eq 0 -and $reads -gt 0) "reads $reads absent $absent bad $bad error '$err'"
    Check 'replace/delete race: the app never failed a rename or delete' ($appErrors -eq 0) "$appErrors app errors"

    # The stand-in UI log, appended by two processes, each line in two writes.
    $ui = Join-Path $work 'ui'
    New-Item -ItemType Directory -Path $ui | Out-Null
    $log = Join-Path $ui 'ui.jsonl'
    $writer = @"
`$log = '$log'
for (`$i = 0; `$i -lt 200; `$i++) {
    `$s = [IO.FileStream]::new(`$log, 'Append', 'Write', 'ReadWrite, Delete')
    `$b = [Text.Encoding]::UTF8.GetBytes('{"event":"start","version":"1.0.' + `$i + '","state":{}}')
    `$s.Write(`$b, 0, `$b.Length); `$s.Flush(); Start-Sleep -Milliseconds 2
    `$s.WriteByte(10); `$s.Dispose()
}
"@
    $w = Start-Fake $writer ''
    $max = 0; $err = ''; $mono = $true
    while (-not $w.HasExited) {
        try {
            $n = (Get-UiStarts $ui).Count
            if ($n -lt $max) { $mono = $false }
            $max = [Math]::Max($max, $n)
        } catch { $err = $_.Exception.Message; break }
    }
    $w.WaitForExit()
    $final = Get-UiStarts $ui
    Check 'ui.jsonl: partial lines are skipped, counts only grow' ($err -eq '' -and $mono) "error '$err'"
    Check 'ui.jsonl: every line read at the end' ($final.Count -eq 200 -and $final[199].version -eq '1.0.199') "$($final.Count)"
    Check 'Wait-UiStart returns the Nth start' ((Wait-UiStart $ui 3 5).version -eq '1.0.2')

    # Array unrolling: zero and one start are still arrays.
    $one = Join-Path $work 'one'
    New-Item -ItemType Directory -Path $one | Out-Null
    Check 'no starts: Count 0' ((Get-UiStarts $one).Count -eq 0)
    [IO.File]::WriteAllText((Join-Path $one 'ui.jsonl'), "{`"event`":`"start`",`"version`":`"1.0.0`"}`n{`"event`":`"start`",`"version`":`"1.0.1`"")
    $s = Get-UiStarts $one
    Check 'one complete start: Count 1, [0] is the event' ($s.Count -eq 1 -and $s[0].version -eq '1.0.0')

    # Missing properties under StrictMode.
    $o = [pscustomobject] @{ a = 1 }
    Check 'Get-Prop: present' ((Get-Prop $o 'a') -eq 1)
    Check 'Get-Prop: missing is $null' ($null -eq (Get-Prop $o 'b'))
    Check 'Get-Prop: on $null is $null' ($null -eq (Get-Prop $null 'b'))
    $home2 = Join-Path $work 'home2'
    New-Item -ItemType Directory -Path (Join-Path $home2 'update') | Out-Null
    [IO.File]::WriteAllText((Join-Path $home2 'update\status.json'), '{"auto_check":true}')
    Check 'status.json without last_update' ($null -eq (Get-LastUpdate $home2))
    Check 'Wait-LastUpdate times out to the current value' ($null -eq (Wait-LastUpdate $home2 'updated' 1))
    [IO.File]::WriteAllText((Join-Path $home2 'update\status.json'), '{"last_update":{"outcome":"updated","to":"1.0.1"}}')
    Check 'Wait-LastUpdate finds the outcome' ((Get-Prop (Wait-LastUpdate $home2 'updated' 5) 'to') -eq '1.0.1')

    # Installed files: version, mismatch, none.
    $dir = Join-Path $work 'install'
    New-Item -ItemType Directory -Path $dir | Out-Null
    Check 'no RELEASE.json' ((Get-FilesVersion $dir) -eq '')
    [IO.File]::WriteAllBytes((Join-Path $dir 'Metro.exe'), [byte[]] (1, 2, 3))
    $h = Get-SharedSha256 (Join-Path $dir 'Metro.exe')
    Check 'shared SHA-256' ($h -eq '039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81') $h
    [IO.File]::WriteAllText((Join-Path $dir 'RELEASE.json'), "{`"version`":`"1.0.1`",`"files`":{`"Metro.exe`":`"$h`"}}")
    Check 'files version' ((Get-FilesVersion $dir) -eq '1.0.1')
    [IO.File]::WriteAllBytes((Join-Path $dir 'Metro.exe'), [byte[]] (9))
    Check 'files mismatch' ((Get-FilesVersion $dir) -eq 'mismatch')

    # Installer idle: no installation and no client naming the scenario's folder.
    $script:FakeProcs = @()
    function Get-CimInstance { param($ClassName, $Filter, $ErrorAction) $script:FakeProcs }
    Check 'no clients: Count 0' ((Get-MsiexecClients 'C:\home').Count -eq 0)
    $t0 = Get-Date
    Wait-InstallerIdle 'C:\home' 20
    $s = ((Get-Date) - $t0).TotalSeconds
    Check 'idle after 3 quiet seconds' ($s -ge 2.9 -and $s -lt 10) "$s s"
    $script:FakeProcs = @([pscustomobject] @{ CommandLine = 'msiexec.exe /i "C:\Home\update\stage-1\package.msi"' }, [pscustomobject] @{ CommandLine = $null })
    Check 'a client of this scenario counts' ((Get-MsiexecClients 'C:\home').Count -eq 1)
    Check 'no marker: no clients' ((Get-MsiexecClients '').Count -eq 0)
    $threw = $false
    try { Wait-InstallerIdle 'C:\home' 2 } catch { $threw = $_.Exception.Message -like '*still busy*' }
    Check 'a running client is never idle' $threw
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
if ($failed) { exit 1 }
exit 0
