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

$out = Join-Path $Root 'out'
$result = [ordered] @{ name = $Name; stage = 'start'; user_sid = ''; scenarios = [Collections.Generic.List[object]]::new()
                     checks = [Collections.Generic.List[object]]::new(); error = '' }
function Record([string] $What, [bool] $Ok, [string] $Detail = '') {
    $result.checks.Add([ordered] @{ name = $What; ok = $Ok; detail = $Detail })
    if (-not $Ok) { throw "check '$What' failed: $Detail" }
}

$spec = Get-Content -LiteralPath (Join-Path $Root 'spec.json') -Raw | ConvertFrom-Json
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
  static string Txt(IntPtr h) { var s = new StringBuilder(256); GetWindowText(h, s, 256); return s.ToString(); }
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

function Invoke-Msiexec([string] $Arguments, [string] $LogName) {
    $log = Join-Path $out $LogName
    $p = Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" -ArgumentList "$Arguments /qn /norestart /l*v `"$log`"" -PassThru
    $null = $p.Handle
    if (-not $p.WaitForExit(600000)) { $p.Kill(); throw "msiexec timed out ($LogName)" }
    return $p.ExitCode
}

function Test-InstallerBusy {
    try { $m = [Threading.Mutex]::OpenExisting('Global\_MSIExecute'); $m.Dispose(); return $true }
    catch [Threading.WaitHandleCannotBeOpenedException] { return $false }
    catch [UnauthorizedAccessException] { return $true }
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

function Read-Ui([string] $Dir) {
    $f = Join-Path $Dir 'ui.jsonl'
    if (-not (Test-Path -LiteralPath $f)) { return @() }
    return @(Get-Content -LiteralPath $f | Where-Object { $_ } | ForEach-Object { $_ | ConvertFrom-Json })
}

# The app replaces or removes these files while the test reads them: a file
# that is gone between the check and the read counts as absent.
function Read-JsonFile([string] $Path) {
    try { $raw = [IO.File]::ReadAllText($Path) }
    catch [IO.FileNotFoundException], [IO.DirectoryNotFoundException] { return $null }
    return $raw | ConvertFrom-Json
}

function Get-Journal([string] $DesktopHome) {
    return Read-JsonFile (Join-Path $DesktopHome 'update\msi-update.json')
}

function Get-LastUpdate([string] $DesktopHome) {
    $s = Read-JsonFile (Join-Path $DesktopHome 'update\status.json')
    if ($s) { return $s.last_update }
    return $null
}

function Get-FilesVersion([string] $Dir) {
    $f = Join-Path $Dir 'RELEASE.json'
    if (-not (Test-Path -LiteralPath $f)) { return '' }
    $m = Get-Content -LiteralPath $f -Raw | ConvertFrom-Json
    $exe = (Get-FileHash -LiteralPath (Join-Path $Dir 'Metro.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($m.files.'Metro.exe' -ne $exe) { return 'mismatch' }
    return [string] $m.version
}

function Stop-Metro([string] $Dir, [string] $DesktopHome) {
    Get-Process -ErrorAction SilentlyContinue | Where-Object {
        $p = $_.Path; $p -and ($p.StartsWith($Dir, 'OrdinalIgnoreCase') -or $p.StartsWith((Join-Path $DesktopHome 'update'), 'OrdinalIgnoreCase'))
    } | Stop-Process -Force -ErrorAction SilentlyContinue
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
    Record "$What`: remembered version and folder" ($reg -and [string] $reg.Version -eq $Version -and ([string] $reg.InstallDir).TrimEnd('\') -eq $Dir) `
        "$(if ($reg) { "$($reg.Version) in $($reg.InstallDir)" })"
    Record "$What`: installed files $Version" ((Get-FilesVersion $Dir) -eq $Version) (Get-FilesVersion $Dir)
}

function Get-ProductVersion([string] $Semver) {
    $v = $Semver.Split('.')
    return "$($v[0]).$($v[1]).$([int] $v[2] * 100 + 99)"
}

# Wait for the Nth UI start in $UiOut (1-based) and return it.
function Wait-UiStart([string] $UiOut, [int] $N, [int] $Seconds) {
    return Wait-Until $Seconds "UI start #$N" { $s = @(Read-Ui $UiOut | Where-Object { $_.event -eq 'start' }); if ($s.Count -ge $N) { $s[$N - 1] } }
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
                $last = Get-LastUpdate $desktopHome
                Record "$tag`: last update 'updated'" ($last -and $last.outcome -eq 'updated' -and $last.to -eq $spec.version_b) ($last | ConvertTo-Json -Compress)
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
                    $last = $s.state.last_update
                    Record "$tag`: About & updates shows install_failed" ($last -and $last.outcome -eq 'install_failed' -and $last.failure_code -eq 'installer_failed') ($last | ConvertTo-Json -Compress)
                    Record "$tag`: update journal retired" (-not (Get-Journal $desktopHome))
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
                $j = Wait-Until 600 'installing' { $j = Get-Journal $desktopHome; if ($j -and $j.phase -eq 'installing') { $j } }
                $helper = Get-Process -Id $j.helper.pid -ErrorAction SilentlyContinue
                Record "$tag`: helper runs from the private copy" ($helper -and $helper.Path.StartsWith((Join-Path $desktopHome 'update\helper-'), 'OrdinalIgnoreCase')) "$(if ($helper) { $helper.Path })"
                $helper | Stop-Process -Force
                if ($Scenario -eq 'helper-msiexec-killed') {
                    $j = Wait-Until 60 'msiexec recorded' { $j = Get-Journal $desktopHome; if ($j -and $j.PSObject.Properties['msiexec']) { $j } else { $null } }
                    Get-Process -Id $j.msiexec.pid -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
                }
                # The helper may die before or after starting msiexec.
                $j = Get-Journal $desktopHome
                if ($j -and $j.PSObject.Properties['msiexec']) {
                    $client = Get-Process -Id $j.msiexec.pid -ErrorAction SilentlyContinue
                    if ($client) { $null = $client.WaitForExit(600000) }
                }
                $null = Wait-Until 600 'Windows Installer to finish' { -not (Test-InstallerBusy) }
                $before = Get-Content -LiteralPath (Join-Path $desktopHome 'update\msi-update.json') -Raw
                $installed = @(Get-Installed)
                $files = Get-FilesVersion $dir
                $p = Start-Metro $dir $desktopHome $uiOut '' 1
                $starts = @(Read-Ui $uiOut | Where-Object { $_.event -eq 'start' }).Count
                $s = $null
                try { $s = Wait-UiStart $uiOut ($starts + 1) 90 } catch { $s = $null }
                if ($s) {
                    $want = if ($installed.Count -eq 1 -and $installed[0] -eq (Get-ProductVersion $spec.version_b)) { $spec.version_b } else { $spec.version_a }
                    Record "$tag`: the next start runs the installed version" ($s.version -eq $want) "started $($s.version), installed $($installed -join ',') files $files"
                    Assert-On $want $dir "$tag (next start)"
                    if ($want -eq $spec.version_b) {
                        $null = Wait-Until 60 'the commit' { -not (Get-Journal $desktopHome) }
                        $entry.outcome = 'continued in B'
                    } else {
                        $last = Get-LastUpdate $desktopHome
                        Record "$tag`: failure recorded" ($last -and $last.outcome -eq 'install_failed') ($last | ConvertTo-Json -Compress)
                        $entry.outcome = 'stayed on A, failure recorded'
                    }
                } else {
                    # Stopped before the workspace: everything kept as it was.
                    $after = Get-Content -LiteralPath (Join-Path $desktopHome 'update\msi-update.json') -Raw
                    Record "$tag`: stopped safely: journal unchanged" ($after -ceq $before)
                    $backups = @(Get-ChildItem -LiteralPath (Join-Path $desktopHome 'update') -Directory -Filter 'db-snapshot-*')
                    Record "$tag`: stopped safely: backup kept" ($backups.Count -eq 1)
                    $entry.outcome = "stopped safely (installed $($installed -join ','), files $files)"
                    $p | Stop-Process -Force -ErrorAction SilentlyContinue
                }
            }
            default { throw "unknown scenario $Scenario" }
        }
        Write-Host "PASS  $tag`: $($entry.outcome)"
    } finally {
        Stop-Metro $dir $desktopHome
        Start-Sleep -Seconds 1
        try { Remove-AllMetro } catch { $result.checks.Add([ordered] @{ name = "$tag`: cleanup"; ok = $false; detail = $_.Exception.Message }) }
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
