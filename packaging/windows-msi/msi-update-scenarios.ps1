<#
  In-app MSI update scenarios, in the current user's own context (started by
  test-msi-update.ps1, directly or as an ephemeral standard account). Each
  scenario installs MSI A per-user, starts Metro A (an e2e test build whose
  stand-in UI presses Update and Restart now), and checks what Windows
  Installer, the registry, the installed files, the update journal and
  Metro's own status say afterwards. It uninstalls again in all cases.

  Scenarios:
    update            A -> B in-app; B starts, commits, the backup is dropped
    failure-retry     B's install fails (its launch condition refuses: a
                      legacy script install is registered); A keeps running
                      and shows install_failed; after removing the cause a
                      second Restart now updates to B
    helper-killed     the helper is killed once msiexec runs; the next
                      start continues in B or records the failure in A,
                      consistent with the facts, or stops with everything kept
    helper-msiexec-killed
                      the helper and the msiexec client are killed; same rule

  Results go to <Root>\out\<Name>.json; logs next to it.
#>
param(
    [Parameter(Mandatory = $true)] [string] $Root,
    [Parameter(Mandatory = $true)] [string] $Name,
    [Parameter(Mandatory = $true)] [string[]] $Scenarios,
    [switch] $OwnEnvironment
)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
# -File passes "a,b" as one string.
$Scenarios = @($Scenarios | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
. (Join-Path $PSScriptRoot 'MsiAutomation.ps1')
. (Join-Path $PSScriptRoot 'MsiHarness.ps1')
. (Join-Path $PSScriptRoot 'UpdateTestIo.ps1')

$out = Join-Path $Root 'out'
$result = [ordered] @{ name = $Name; stage = 'start'; user_sid = ''; scenarios = [Collections.Generic.List[object]]::new()
                     checks = [Collections.Generic.List[object]]::new(); error = '' }
function Record([string] $What, [bool] $Ok, [string] $Detail = '') {
    $result.checks.Add([ordered] @{ name = $What; ok = $Ok; detail = $Detail })
    if (-not $Ok) { throw "check '$What' failed: $Detail" }
}

$spec = Read-SharedJson (Join-Path $Root 'spec.json')
$LegacyKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\dev.metro.desktop'
$RegKey = 'HKCU:\Software\Metro\Desktop MSI'

# Windows Installer's basic UI (/passive) shows its errors (here: B's
# refused launch condition) in a message box. Click OK on msiexec message
# boxes only, from a separate job, so the failure can complete unattended.
$DismissSource = @'
using System; using System.Diagnostics; using System.Runtime.InteropServices; using System.Text;
public static class MetroMsiDialogs {
  delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr p, EnumProc f, IntPtr l);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  static string Cls(IntPtr h) { var s = new StringBuilder(256); GetClassName(h, s, 256); return s.ToString(); }
  static string Txt(IntPtr h) { var s = new StringBuilder(4096); GetWindowText(h, s, 4096); return s.ToString(); }
  // The text of a visible dialog box owned by process `pid` (Metro's fatal
  // message), or null.
  public static string DialogText(int pid) {
    string found = null;
    EnumWindows((h, l) => {
      uint owner; GetWindowThreadProcessId(h, out owner);
      if (owner != (uint) pid || !IsWindowVisible(h) || Cls(h) != "#32770") return true;
      var text = new StringBuilder();
      EnumChildWindows(h, (c, l2) => { if (Cls(c) == "Static") text.Append(Txt(c)); return true; }, IntPtr.Zero);
      found = text.ToString();
      return false; }, IntPtr.Zero);
    return found;
  }
  public static int ClickOk() {
    int n = 0;
    EnumWindows((h, l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      string name = "";
      try { name = Process.GetProcessById((int) pid).ProcessName; } catch { }
      if (!IsWindowVisible(h) || Cls(h) != "#32770" || !name.Equals("msiexec", StringComparison.OrdinalIgnoreCase)) return true;
      IntPtr ok = IntPtr.Zero; int buttons = 0;
      EnumChildWindows(h, (c, l2) => {
        if (Cls(c) == "Button") { buttons++; string t = Txt(c).Replace("&", ""); if (t == "OK") ok = c; }
        return true; }, IntPtr.Zero);
      // A message box with exactly one button, OK (never the progress window's Cancel).
      if (ok != IntPtr.Zero && buttons == 1) { SendMessage(ok, 0x00F5, IntPtr.Zero, IntPtr.Zero); n++; }
      return true; }, IntPtr.Zero);
    return n;
  }
}
'@

Add-Type -TypeDefinition $DismissSource

function Start-DialogDismisser([string] $Log) {
    return Start-Job -ArgumentList $DismissSource, $Log -ScriptBlock {
        param($Source, $Log)
        Add-Type -TypeDefinition $Source
        while ($true) {
            $n = [MetroMsiDialogs]::ClickOk()
            if ($n -gt 0) { Add-Content -LiteralPath $Log -Value "$(Get-Date -Format o) clicked OK on $n msiexec message box(es)" }
            Start-Sleep -Milliseconds 500
        }
    }
}

# The test's own installs. 1618 (another installation is running): wait
# until Windows Installer is idle and try again.
function Invoke-Msiexec([string] $Arguments, [string] $LogName) {
    for ($try = 1; ; $try++) {
        $log = Join-Path $out $(if ($try -eq 1) { $LogName } else { "$LogName.$try" })
        $p = Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" -ArgumentList "$Arguments /qn /norestart /l*v `"$log`"" -PassThru
        $null = $p.Handle
        if (-not $p.WaitForExit(600000)) { $p.Kill(); throw "msiexec timed out ($LogName)" }
        if ($p.ExitCode -ne 1618 -or $try -ge 3) { return $p.ExitCode }
        Wait-InstallerIdle '' 300
    }
}

# The same facts the app checks: the _MSIExecute mutex (access denied counts
# as busy) and a suspended installation (HKLM ...\Installer\InProgress).
function Test-InstallerBusy {
    if (Test-Path -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Installer\InProgress') { return $true }
    try { $m = [Threading.Mutex]::OpenExisting('Global\_MSIExecute'); $m.Dispose(); return $true }
    catch [Threading.WaitHandleCannotBeOpenedException] { return $false }
    catch [UnauthorizedAccessException] { return $true }
}

# msiexec client processes whose command line names $Marker (a scenario's
# folder); '' matches none.
function Get-MsiexecClients([string] $Marker) {
    if (-not $Marker) { return , @() }
    return , @(Get-CimInstance Win32_Process -Filter "Name = 'msiexec.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -and $_.CommandLine.IndexOf($Marker, [StringComparison]::OrdinalIgnoreCase) -ge 0 })
}

# Idle for 3 s in a row: no installation and no client of this scenario.
function Wait-InstallerIdle([string] $Marker, [int] $Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    $since = $null
    while ((Get-Date) -lt $deadline) {
        if ((Test-InstallerBusy) -or (Get-MsiexecClients $Marker).Count) { $since = $null }
        elseif (-not $since) { $since = Get-Date }
        elseif (((Get-Date) - $since).TotalSeconds -ge 3) { return }
        Start-Sleep -Milliseconds 200
    }
    throw "Windows Installer still busy after $Seconds s"
}

function Wait-Until([int] $Seconds, [string] $What, [scriptblock] $Probe) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $v = & $Probe
        if ($v) { return $v }
        Start-Sleep -Milliseconds 100
    }
    throw "timed out after $Seconds s waiting for $What"
}

function Get-Installed {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $codes = Get-RelatedProducts $installer ([string] $spec.upgrade_code)
    return @(foreach ($c in $codes) { [string] (Invoke-MsiAutomation $installer 'Installer.ProductInfo' @($c, 'VersionString')) })
}

function Remove-AllMetro {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    foreach ($c in (Get-RelatedProducts $installer ([string] $spec.upgrade_code))) {
        $code = Invoke-Msiexec "/x $c" "uninstall-$c.log"
        if ($code -ne 0) { throw "uninstall of $c exited $code" }
    }
}

# Every start event of the stand-in UI so far (always an array).
function Get-UiStarts([string] $Dir) {
    $lines = Read-SharedJsonLines (Join-Path $Dir 'ui.jsonl')
    return , @($lines | Where-Object { (Get-Prop $_ 'event') -eq 'start' })
}

function Get-JournalPath([string] $DesktopHome) { return Join-Path $DesktopHome 'update\msi-update.json' }
function Get-Journal([string] $DesktopHome) { return Read-SharedJson (Get-JournalPath $DesktopHome) }
function Get-LastUpdate([string] $DesktopHome) {
    return Get-Prop (Read-SharedJson (Join-Path $DesktopHome 'update\status.json')) 'last_update'
}

function Get-FilesVersion([string] $Dir) {
    $m = Read-SharedJson (Join-Path $Dir 'RELEASE.json')
    if (-not $m) { return '' }
    if ([string] (Get-Prop (Get-Prop $m 'files') 'Metro.exe') -ne (Get-SharedSha256 (Join-Path $Dir 'Metro.exe'))) { return 'mismatch' }
    return [string] (Get-Prop $m 'version')
}

# Kill Metro (launcher, UI, helper copy) and wait until each one has exited,
# so nothing holds the installed files when the uninstall starts.
function Stop-Metro([string] $Dir, [string] $DesktopHome) {
    $procs = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
        $p = $_.Path; $p -and ($p.StartsWith($Dir, 'OrdinalIgnoreCase') -or $p.StartsWith((Join-Path $DesktopHome 'update'), 'OrdinalIgnoreCase'))
    })
    foreach ($p in $procs) { try { $p.Kill() } catch [InvalidOperationException] { } catch [ComponentModel.Win32Exception] { } }
    foreach ($p in $procs) { $null = $p.WaitForExit(30000) }
}

function Start-Metro([string] $Dir, [string] $DesktopHome, [string] $UiOut, [string] $To, [int] $Attempts) {
    $env:METRO_DESKTOP_HOME = $DesktopHome
    $env:METRO_TEST_RELEASE_BASE = [string] $spec.release_base
    $env:METRO_MSI_TEST_OUT = $UiOut
    $env:METRO_MSI_TEST_UPDATE_TO = $To
    $env:METRO_MSI_TEST_ATTEMPTS = [string] $Attempts
    $p = Start-Process -FilePath (Join-Path $Dir 'Metro.exe') -WorkingDirectory $Dir -PassThru
    $null = $p.Handle
    return $p
}

function Assert-On([string] $Version, [string] $Dir, [string] $What) {
    $installed = @(Get-Installed)
    Record "$What`: product $Version" ($installed.Count -eq 1 -and $installed[0] -eq (Get-ProductVersion $Version)) ($installed -join ', ')
    $reg = Get-ItemProperty -LiteralPath $RegKey -ErrorAction SilentlyContinue
    $regVersion, $regDir = [string] (Get-Prop $reg 'Version'), [string] (Get-Prop $reg 'InstallDir')
    Record "$What`: remembered version and folder" ($regVersion -eq $Version -and $regDir.TrimEnd('\') -eq $Dir) "$regVersion in $regDir"
    Record "$What`: installed files $Version" ((Get-FilesVersion $Dir) -eq $Version) (Get-FilesVersion $Dir)
}

function Get-ProductVersion([string] $Semver) {
    $v = $Semver.Split('.')
    return "$($v[0]).$($v[1]).$([int] $v[2] * 100 + 99)"
}

# Wait for the Nth UI start in $UiOut (1-based) and return it.
function Wait-UiStart([string] $UiOut, [int] $N, [int] $Seconds) {
    return Wait-Until $Seconds "UI start #$N" { $s = Get-UiStarts $UiOut; if ($s.Count -ge $N) { $s[$N - 1] } }
}

function Wait-LastUpdate([string] $DesktopHome, [string] $Outcome, [int] $Seconds) {
    try { return Wait-Until $Seconds "last update '$Outcome'" { $l = Get-LastUpdate $DesktopHome; if ((Get-Prop $l 'outcome') -eq $Outcome) { $l } } }
    catch { return Get-LastUpdate $DesktopHome }
}

function Invoke-Scenario([string] $Scenario) {
    $tag = $Scenario
    $base = Join-Path $script:Programs "Metro Update Test $tag"
    $dir = Join-Path $base 'Metro Desktop'
    $desktopHome = Join-Path $script:Local "Metro Update Test $tag\Desktop"
    $uiOut = Join-Path $out "ui-$tag"
    New-Item -ItemType Directory -Force -Path $uiOut | Out-Null
    $entry = [ordered] @{ scenario = $Scenario; outcome = '' }
    $result.scenarios.Add($entry)
    try {
        Record "$tag`: no Metro product before" (@(Get-Installed).Count -eq 0)
        $code = Invoke-Msiexec "/i `"$(Join-Path $Root $spec.msi_a)`" INSTALLDIR=`"$dir`"" "$tag-install-a.log"
        Record "$tag`: MSI A installed" ($code -eq 0) "exit $code"
        Assert-On $spec.version_a $dir "$tag (before)"
        switch ($Scenario) {
            'update' {
                $null = Start-Metro $dir $desktopHome $uiOut $spec.version_b 1
                $s = Wait-UiStart $uiOut 2 600
                Record "$tag`: Metro B started" ($s.version -eq $spec.version_b) "started $($s.version)"
                $null = Wait-Until 60 'the commit' { -not (Get-Journal $desktopHome) }
                Assert-On $spec.version_b $dir "$tag (after)"
                $last = Wait-LastUpdate $desktopHome 'updated' 60
                Record "$tag`: last update 'updated'" ((Get-Prop $last 'outcome') -eq 'updated' -and (Get-Prop $last 'to') -eq $spec.version_b) ($last | ConvertTo-Json -Compress)
                $left = @(Get-ChildItem -LiteralPath (Join-Path $desktopHome 'update') -Directory | Where-Object { $_.Name -like 'db-snapshot-*' -or $_.Name -like 'stage-*' })
                Record "$tag`: backup and stage dropped" ($left.Count -eq 0) (($left | ForEach-Object Name) -join ', ')
                $entry.outcome = 'updated'
            }
            'failure-retry' {
                New-Item -Path $LegacyKey -Force | Out-Null
                Set-ItemProperty -LiteralPath $LegacyKey -Name InstallLocation -Value (Join-Path $base 'legacy')
                $dismisser = Start-DialogDismisser (Join-Path $out "$tag-dialogs.log")
                try {
                    $null = Start-Metro $dir $desktopHome $uiOut $spec.version_b 1
                    $s = Wait-UiStart $uiOut 2 600
                    Record "$tag`: Metro A started again" ($s.version -eq $spec.version_a) "started $($s.version)"
                    $last = Get-Prop (Get-Prop $s 'state') 'last_update'
                    Record "$tag`: About & updates shows install_failed" ((Get-Prop $last 'outcome') -eq 'install_failed' -and (Get-Prop $last 'failure_code') -eq 'installer_failed') ($last | ConvertTo-Json -Compress)
                    $retired = try { Wait-Until 30 'the journal to retire' { -not (Get-Journal $desktopHome) } } catch { $false }
                    Record "$tag`: update journal retired" ([bool] $retired)
                    Assert-On $spec.version_a $dir "$tag (after the failure)"
                    $null = Wait-Until 60 'Metro A to exit' { -not (Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($dir, 'OrdinalIgnoreCase') }) }
                } finally {
                    $dismisser | Stop-Job -PassThru | Remove-Job -Force
                    Remove-Item -LiteralPath $LegacyKey -Recurse -Force -ErrorAction SilentlyContinue
                }
                $null = Start-Metro $dir $desktopHome $uiOut $spec.version_b 2
                $s = Wait-UiStart $uiOut 4 600
                Record "$tag`: retry: Metro B started" ($s.version -eq $spec.version_b) "started $($s.version)"
                $null = Wait-Until 60 'the commit' { -not (Get-Journal $desktopHome) }
                Assert-On $spec.version_b $dir "$tag (after the retry)"
                $entry.outcome = 'failed, then updated'
            }
            { $_ -in 'helper-killed', 'helper-msiexec-killed' } {
                $null = Start-Metro $dir $desktopHome $uiOut $spec.version_b 1
                # Mid-msiexec: the helper has recorded its msiexec client.
                $j = Wait-Until 600 'msiexec under the helper' {
                    $j = Get-Journal $desktopHome
                    $phase = Get-Prop $j 'phase'
                    if ($phase -in 'installed', 'committed', 'failed') { throw "the update reached '$phase' before the kill" }
                    if ($phase -eq 'installing' -and (Get-Prop $j 'msiexec')) { $j }
                }
                $helper = Get-Process -Id $j.helper.pid -ErrorAction SilentlyContinue
                $client = Get-Process -Id $j.msiexec.pid -ErrorAction SilentlyContinue
                Record "$tag`: helper runs from the private copy" ($helper -and $helper.Path -and $helper.Path.StartsWith((Join-Path $desktopHome 'update\helper-'), 'OrdinalIgnoreCase')) "$(if ($helper) { $helper.Path })"
                $victims = @($helper) + $(if ($Scenario -eq 'helper-msiexec-killed') { @($client) } else { @() })
                foreach ($v in $victims) { if ($v) { try { $v.Kill() } catch [InvalidOperationException] { } } }
                foreach ($v in $victims) { if ($v) { $null = $v.WaitForExit(30000) } }
                # The kill must have landed before the helper finished: the
                # journal still says installing.
                $after = Get-Journal $desktopHome
                Record "$tag`: killed while installing" ((Get-Prop $after 'phase') -eq 'installing') "phase $(Get-Prop $after 'phase')"
                if ($Scenario -eq 'helper-killed' -and $client) { $null = $client.WaitForExit(600000) }
                Wait-InstallerIdle $desktopHome 600
                $before = Read-SharedText (Get-JournalPath $desktopHome)
                Record "$tag`: journal kept for the next start" ($null -ne $before)
                $installed = @(Get-Installed)
                $files = Get-FilesVersion $dir
                $starts = (Get-UiStarts $uiOut).Count
                $p = Start-Metro $dir $desktopHome $uiOut '' 1
                # The next start either runs a version (a UI start), stops with
                # its message box, or exits; nothing else counts.
                $next = Wait-Until 120 'the next start to decide' {
                    $s = Get-UiStarts $uiOut
                    if ($s.Count -gt $starts) { return @{ kind = 'start'; start = $s[$starts] } }
                    $text = [MetroMsiDialogs]::DialogText($p.Id)
                    if ($null -ne $text) { return @{ kind = 'stopped'; text = $text } }
                    if ($p.HasExited) { return @{ kind = 'exited'; code = $p.ExitCode } }
                }
                $facts = "installed $($installed -join ','), files $files"
                switch ($next.kind) {
                    'start' {
                        $s = $next.start
                        $want = if ($installed.Count -eq 1 -and $installed[0] -eq (Get-ProductVersion $spec.version_b)) { $spec.version_b } else { $spec.version_a }
                        Record "$tag`: the next start runs the installed version" ($s.version -eq $want) "started $($s.version), $facts"
                        Assert-On $want $dir "$tag (next start)"
                        if ($want -eq $spec.version_b) {
                            $null = Wait-Until 60 'the commit' { -not (Get-Journal $desktopHome) }
                            $entry.outcome = "continued in B ($facts)"
                        } else {
                            $last = Wait-LastUpdate $desktopHome 'install_failed' 60
                            Record "$tag`: failure recorded" ((Get-Prop $last 'outcome') -eq 'install_failed') ($last | ConvertTo-Json -Compress)
                            $entry.outcome = "stayed on A, failure recorded ($facts)"
                        }
                    }
                    'stopped' {
                        # Stopped before the workspace: everything kept as it was.
                        Record "$tag`: stopped safely: journal unchanged" ((Read-SharedText (Get-JournalPath $desktopHome)) -ceq $before)
                        $backups = @(Get-ChildItem -LiteralPath (Join-Path $desktopHome 'update') -Directory -Filter 'db-snapshot-*')
                        Record "$tag`: stopped safely: backup kept" ($backups.Count -eq 1)
                        $entry.outcome = "stopped safely ($facts): $(($next.text -split "`n")[0])"
                    }
                    default { Record "$tag`: the next start decided" $false "exited with $($next.code) without a UI or a message ($facts)" }
                }
            }
            default { throw "unknown scenario $Scenario" }
        }
        Write-Host "PASS  $tag`: $($entry.outcome)"
    } finally {
        Stop-Metro $dir $desktopHome
        try { Wait-InstallerIdle $desktopHome 300; Remove-AllMetro } catch { $result.checks.Add([ordered] @{ name = "$tag`: cleanup"; ok = $false; detail = $_.Exception.Message }) }
        Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
        $logs = Join-Path $desktopHome 'update\msiexec.log'
        if (Test-Path -LiteralPath $logs) { Copy-Item -LiteralPath $logs -Destination (Join-Path $out "$tag-update-msiexec.log") -Force }
    }
}

try {
    $result.user_sid = ([string] (& "$env:SystemRoot\System32\whoami.exe" /user /fo csv /nh) | ConvertFrom-Csv -Header Name, Sid).Sid
    if ($OwnEnvironment) {
        $result.stage = 'own environment'
        $groups = @(& "$env:SystemRoot\System32\whoami.exe" /groups /fo csv /nh)
        $problem = Get-NonAdminTokenProblem $groups
        Record 'token is standard' ($problem -eq '') $problem
        $own = Initialize-OwnUserEnvironment $result.user_sid
        $script:Local = $own.Folders.LocalAppData
        $script:Programs = Join-Path $own.Folders.LocalAppData 'Programs'
    } else {
        $script:Local = [Environment]::GetFolderPath('LocalApplicationData')
        $script:Programs = Join-Path $script:Local 'Programs'
    }
    Record 'no legacy script install registered' (-not (Test-Path -LiteralPath $LegacyKey))
    foreach ($s in $Scenarios) {
        $result.stage = $s
        Invoke-Scenario $s
    }
} catch {
    $result.error = "stage '$($result.stage)', $(Format-FailureLocation $_): $($_.Exception.Message)"
} finally {
    $json = $result | ConvertTo-Json -Depth 8
    [IO.File]::WriteAllText((Join-Path $out "$Name.json"), $json, [Text.UTF8Encoding]::new($false))
}
if ($result.error) { exit 1 }
exit 0
