<#
.SYNOPSIS
  Native Windows checks for Metro's per-user MSI. CI only: it installs and
  removes per-user products in the current Windows profile.

.DESCRIPTION
  Part 1 (always): the "Metro MSI test fixture" product built by
  `build_msi.py fixtures` (its own UpgradeCode, folder, registry key and
  shortcut, never released) goes through msiexec /qn:
    custom folder with spaces, marker and Start menu shortcut on by default,
    upgrade into the remembered folder with LOCALRUNNER=0, repair keeps the
    choice, downgrade refused (1603), STARTMENUSHORTCUT=0, same-version
    different build refused unless METRO_REPLACE_SAME_VERSION=1, legacy
    install-metro-windows.ps1 install refused, uninstall keeps data and
    unrelated files, default %LOCALAPPDATA%\Programs folder, another drive
    (D:) when present, and (with -StandardUserFixture, GitHub-hosted runners
    only) an ephemeral standard local account that installs, inspects and
    uninstalls in its own context, policy-aware (see step 11).
  Part 2 (with -Msi): the real Metro MSI (signed or unsigned) installs into a
  custom folder with spaces, every installed file matches the installed
  RELEASE.json, the marker and shortcut follow the options, and uninstall
  keeps Metro data and unrelated files.

  These checks drive msiexec without UI. The wizard itself (Browse dialog) is
  covered by static table checks (build_msi.py inspect), not here.

.EXAMPLE
  pwsh -File test-msi-windows.ps1 -FixturesDir .\fixtures -Msi .\Metro-0.3.0-windows-x64.msi -Version 0.3.0 -RequireFile metro-tui.exe -StandardUserFixture
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $FixturesDir,
    [string] $Msi,
    [string] $Version,
    [string[]] $RequireFile = @(),
    [string] $LogDir,
    [switch] $IAcceptChangesToThisWindowsProfile,
    # GitHub-hosted CI only: an ephemeral standard account (and DisableMsi=0 when the
    # machine policy blocks per-user installs) for the non-admin install (step 11).
    [switch] $StandardUserFixture
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
if ([System.Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'test-msi-windows.ps1 runs on Windows only' }
if (-not ($env:CI -eq 'true' -or $IAcceptChangesToThisWindowsProfile)) {
    throw 'This test installs and removes per-user products in this Windows profile. Run it in CI (CI=true) or pass -IAcceptChangesToThisWindowsProfile.'
}

. (Join-Path $PSScriptRoot 'MsiAutomation.ps1')
. (Join-Path $PSScriptRoot 'MsiHarness.ps1')
# Parameters are script variables; PowerShell names are case-insensitive, so no
# script-level variable may reuse a parameter name (an earlier `$msi = @{}`
# silently became the [string] -Msi). test-msi-harness.ps1 checks this.
$Installer = New-Object -ComObject WindowsInstaller.Installer
$Fixtures = Get-Content -LiteralPath (Join-Path $FixturesDir 'fixtures.json') -Raw | ConvertFrom-Json
$LocalAppData = [Environment]::GetFolderPath('LocalApplicationData')
$Programs = [Environment]::GetFolderPath('Programs')
if (-not $LogDir) { $LogDir = Join-Path ([System.IO.Path]::GetTempPath()) ('metro-msi-logs-' + [Guid]::NewGuid().ToString('N')) }
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
# Long-form path with spaces (GetTempPath can be an 8.3 short path such as RUNNER~1,
# which would not compare equal to the long paths Windows reports back).
$Work = Join-Path $LocalAppData ('Metro MSI Test ' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $Work | Out-Null
$script:Step = 0
$script:Failed = $false
$script:Results = New-Object System.Collections.Generic.List[string]

function Pass([string] $What) { $script:Results.Add("PASS  $What"); Write-Host "PASS  $What" }
function Skip([string] $What) { $script:Results.Add("SKIP  $What"); Write-Host "SKIP  $What" }
function Check([bool] $Cond, [string] $What) { if (-not $Cond) { throw "FAIL  $What" } }

function Invoke-Msi([string] $Arguments, [int] $Expect, [string] $What) {
    $script:Step++
    $log = Join-Path $LogDir ('{0:D2}-msiexec.log' -f $script:Step)
    $p = Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" -ArgumentList "$Arguments /qn /norestart /l*v `"$log`"" -PassThru
    $null = $p.Handle  # cache the handle so ExitCode is available after the wait
    if (-not $p.WaitForExit(600000)) { $p.Kill(); throw "FAIL  msiexec timed out: $What (log $log)" }
    if ($p.ExitCode -ne $Expect) { throw "FAIL  $What`: msiexec exit $($p.ExitCode), expected $Expect (log $log)" }
    Pass "$What (msiexec exit $($p.ExitCode))"
    return $log
}

function Get-Lnk([string] $Name) { Join-Path $Programs $Name }

function Get-LnkTarget([string] $Path) { (New-Object -ComObject WScript.Shell).CreateShortcut($Path).TargetPath }

function Get-Sha([string] $Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }

# The installed tree must be exactly the payload (+ marker when on) + any $Extra files.
function Assert-Installed([string] $Dir, $Files, [bool] $Marker, [string[]] $Extra, [string] $What) {
    $problems = Get-InstalledTreeProblems $Dir $Files $Marker $Extra
    Check ($problems.Count -eq 0) "$What`: $($problems -join '; ')"
    Pass "$What`: files exact (marker $(if ($Marker) {'on'} else {'off'}))"
}

# Runs standard-user-msi.ps1 as $Credential (profile loaded) and returns its parsed result.
function Invoke-StandardUserPhase([pscredential] $Credential, [string] $Root, [string] $Phase, [string] $Expect) {
    $pwsh = Join-Path $PSHOME 'pwsh.exe'
    $childScript = Join-Path $Root 'standard-user-msi.ps1'
    $p = Start-Process -FilePath $pwsh -Credential $Credential -LoadUserProfile -WorkingDirectory $Root -WindowStyle Hidden -PassThru `
        -ArgumentList "-NoProfile -NonInteractive -File `"$childScript`" -Root `"$Root`" -Phase $Phase -Expect $Expect"
    $null = $p.Handle
    if (-not $p.WaitForExit(900000)) { $p.Kill(); throw "FAIL  standard-user $Phase phase timed out" }
    $file = Join-Path $Root "out\$Phase.json"
    $result = if (Test-Path -LiteralPath $file) { Get-Content -LiteralPath $file -Raw | ConvertFrom-Json } else { $null }
    Write-Host ("standard-user {0} phase: exit {1} (0x{1:X8}){2}" -f $Phase, $p.ExitCode, $(if ($result) { '' } else { '; it wrote no result' }))
    return $result
}

# Ephemeral standard local account (Users only), its profile and a test root
# readable by it; removed again in all cases. Only on a GitHub-hosted runner.
function Invoke-StandardUserMsiFixture([string] $Package, $Spec, [string] $Expectation, $PolicyStore) {
    $name = 'metromsi' + [Guid]::NewGuid().ToString('N').Substring(0, 8)
    $root = Join-Path $env:SystemDrive ('MetroMsiStdUser-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    $sid = $null
    $failure = $null
    $password = $null
    try {
        $password = New-EphemeralPassword   # SecureString only; never written or printed
        $user = New-LocalUser -Name $name -Password $password -AccountNeverExpires -PasswordNeverExpires `
            -UserMayNotChangePassword -Description 'Metro MSI CI test account (ephemeral)'
        $sid = $user.SID.Value
        # Users only (interactive logon, nothing more); New-LocalUser may already have added it.
        if (-not @(Get-LocalGroupMember -SID 'S-1-5-32-545' | Where-Object { $_.SID.Value -eq $sid }).Count) {
            Add-LocalGroupMember -SID 'S-1-5-32-545' -Member $sid
        }
        $admins = @(Get-LocalGroupMember -SID 'S-1-5-32-544' | Where-Object { $_.SID.Value -eq $sid })
        Check ($admins.Count -eq 0) 'the test account is not an administrator'
        Pass "ephemeral standard account created (SID $sid; Users only)"
        # Test root: SYSTEM and Administrators full control, the test account read
        # (and modify only on out\); no inherited access.
        New-Item -ItemType Directory -Path $root | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $root 'out') | Out-Null
        foreach ($f in 'standard-user-msi.ps1', 'MsiAutomation.ps1', 'MsiHarness.ps1') { Copy-Item -LiteralPath (Join-Path $PSScriptRoot $f) -Destination $root }
        Copy-Item -LiteralPath $Package -Destination (Join-Path $root ([string] $Spec.package))
        [IO.File]::WriteAllText((Join-Path $root 'spec.json'), ($Spec | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
        $acl = Get-Acl -LiteralPath $root
        $acl.SetAccessRuleProtection($true, $false)
        foreach ($rule in @($acl.Access)) { $null = $acl.RemoveAccessRuleSpecific($rule) }
        $inherit = [Security.AccessControl.InheritanceFlags] 'ContainerInherit, ObjectInherit'
        $none = [Security.AccessControl.PropagationFlags]::None
        foreach ($admin in 'S-1-5-18', 'S-1-5-32-544') {
            $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new($admin), 'FullControl', $inherit, $none, 'Allow'))
        }
        $userSid = [Security.Principal.SecurityIdentifier]::new($sid)
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($userSid, 'ReadAndExecute', $inherit, $none, 'Allow'))
        Set-Acl -LiteralPath $root -AclObject $acl
        $outAcl = Get-Acl -LiteralPath (Join-Path $root 'out')
        $outAcl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($userSid, 'Modify', $inherit, $none, 'Allow'))
        Set-Acl -LiteralPath (Join-Path $root 'out') -AclObject $outAcl
        $cred = [pscredential]::new("$env:COMPUTERNAME\$name", $password)

        $r = Invoke-StandardUserPhase $cred $root 'host' $Expectation
        $problems = Get-StandardUserResultProblems $r $sid $Expectation
        Check ($problems.Count -eq 0) "standard user under this machine's policy: $($problems -join '; ')"
        Pass "standard user under this machine's policy: $(if ($Expectation -eq 'refused') { 'refused (1625), nothing registered or installed' } else { 'installed, inspected and uninstalled its own product' })"
        if ($Expectation -eq 'refused') {
            Invoke-WithClientMsiPolicy $PolicyStore {
                $r2 = Invoke-StandardUserPhase $cred $root 'client' 'allowed'
                $problems2 = Get-StandardUserResultProblems $r2 $sid 'allowed'
                Check ($problems2.Count -eq 0) "standard user with DisableMsi=0: $($problems2 -join '; ')"
            }
            Pass 'standard user with DisableMsi=0 (client policy): installed, inspected and uninstalled its own product'
            Pass "installer policy restored: DisableMsi $(Format-RegistryValueState (Get-RegistryValueState $PolicyStore $script:MsiPolicyKey $script:MsiPolicyValue))"
        }
    } catch { $failure = $_ } finally {
        if (Test-Path -LiteralPath (Join-Path $root 'out')) {
            Copy-Item -Path (Join-Path $root 'out\*') -Destination $LogDir -Force -ErrorAction SilentlyContinue
        }
        if ($password) { $password.Dispose() }
        $cleanup = [Collections.Generic.List[string]]::new()
        if ($sid) {
            Get-Process -IncludeUserName -ErrorAction SilentlyContinue | Where-Object { $_.UserName -like "*\$name" } |
                Stop-Process -Force -ErrorAction SilentlyContinue
            $deadline = (Get-Date).AddSeconds(90)
            do {
                $prof = Get-CimInstance -ClassName Win32_UserProfile -Filter "SID = '$sid'"
                if (-not $prof) { break }
                if (-not $prof.Loaded) { $prof | Remove-CimInstance -ErrorAction SilentlyContinue }
                Start-Sleep -Seconds 2
            } while ((Get-Date) -lt $deadline)
            if (Get-CimInstance -ClassName Win32_UserProfile -Filter "SID = '$sid'") { $cleanup.Add('the test profile could not be removed') }
        }
        if (Get-LocalUser -Name $name -ErrorAction SilentlyContinue) { Remove-LocalUser -Name $name }
        if (Get-LocalUser -Name $name -ErrorAction SilentlyContinue) { $cleanup.Add('the test account could not be removed') }
        if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue }
        if (Test-Path -LiteralPath $root) { $cleanup.Add('the test root could not be removed') }
        if ($cleanup.Count -gt 0) {
            if ($failure) { Write-Host "standard-user cleanup also failed: $($cleanup -join '; ')" }
            else { throw "FAIL  standard-user cleanup: $($cleanup -join '; ')" }
        } elseif ($sid) { Pass 'ephemeral account, its profile and the test root removed' }
    }
    if ($failure) { throw $failure }
}

function Assert-Single([string] $UpgradeCode, [string] $ProductCode, [string] $What) {
    $rel = Get-RelatedProducts $Installer $UpgradeCode
    Check ($rel.Count -eq 1 -and $rel[0] -eq $ProductCode) "$What`: expected only $ProductCode installed, got $($rel -join ', ')"
    $facts = Get-ProductFacts $Installer $ProductCode
    Check ($facts.AssignmentType -eq '0') "$What`: Windows Installer must report a per-user installation (AssignmentType $($facts.AssignmentType))"
    Pass "$What`: one per-user product $ProductCode"
}

try {
    $uc = [string] $Fixtures.upgrade_code
    $fx = $Fixtures.versions
    $pc = @{}; foreach ($p in $fx.PSObject.Properties) { $pc[$p.Name] = [string] $p.Value.product_code }
    $fixtureMsi = @{}; foreach ($p in $fx.PSObject.Properties) { $fixtureMsi[$p.Name] = Join-Path $FixturesDir ([string] $p.Value.file) }
    $lnk = Get-Lnk ([string] $Fixtures.shortcut)
    $regKey = 'HKCU:\' + [string] $Fixtures.reg_key
    $legacyKey = 'HKCU:\' + [string] $Fixtures.legacy_key
    Check ((Get-RelatedProducts $Installer $uc).Count -eq 0) 'fixture product must not be installed before the test'
    Check (-not (Test-Path $legacyKey)) 'fixture legacy key must not exist before the test'

    # 1. Fresh install into a custom folder with spaces; defaults: marker + shortcut on.
    $custom = Join-Path $Work 'Custom Folder With Spaces\Metro'
    $data = Join-Path $LocalAppData 'Metro MSI Fixture Data'
    New-Item -ItemType Directory -Force -Path $data | Out-Null
    Set-Content -LiteralPath (Join-Path $data 'keep.txt') -Value 'keep' -Encoding ASCII
    Invoke-Msi "/i `"$($fixtureMsi['1.0.0'])`" INSTALLDIR=`"$custom`"" 0 'install 1.0.0 into a custom folder with spaces' | Out-Null
    Assert-Installed $custom $fx.'1.0.0'.files $true @() '1.0.0 custom folder'
    Check (Test-Path -LiteralPath $lnk) 'Start menu shortcut created by default'
    Check ((Get-LnkTarget $lnk) -eq (Join-Path $custom 'Metro.exe')) 'shortcut points into the chosen folder'
    Assert-Single $uc $pc['1.0.0'] '1.0.0'
    Check ((Get-ProductFacts $Installer $pc['1.0.0']).InstallLocation.TrimEnd('\') -eq $custom) 'Windows Installer InstallLocation is the chosen folder'
    Check ((Get-ItemProperty $regKey).InstallDir.TrimEnd('\') -eq $custom) 'chosen folder remembered'
    Pass 'shortcut, InstallLocation and remembered folder'
    Set-Content -LiteralPath (Join-Path $custom 'user-notes.txt') -Value 'mine' -Encoding ASCII
    New-Item -ItemType Directory -Force -Path (Join-Path $custom 'ui\my-stuff') | Out-Null
    Set-Content -LiteralPath (Join-Path $custom 'ui\my-stuff\x.txt') -Value 'mine' -Encoding ASCII
    $extra = @('user-notes.txt', 'ui/my-stuff/x.txt')

    # 2. Upgrade (no INSTALLDIR: the remembered folder) with the local runner off.
    Invoke-Msi "/i `"$($fixtureMsi['1.1.0'])`" LOCALRUNNER=0" 0 'upgrade 1.0.0 -> 1.1.0 with LOCALRUNNER=0' | Out-Null
    Assert-Installed $custom $fx.'1.1.0'.files $false $extra '1.1.0 same folder, unrelated files kept'
    Assert-Single $uc $pc['1.1.0'] '1.1.0 replaced 1.0.0'
    Check (Test-Path -LiteralPath $lnk) 'shortcut kept on upgrade'
    Check ((Get-Content -LiteralPath (Join-Path $data 'keep.txt')) -eq 'keep') 'data kept on upgrade'

    # 3. Repair/reinstall of the same package keeps the previous (off) choice.
    Invoke-Msi "/fomus `"$($fixtureMsi['1.1.0'])`"" 0 'repair 1.1.0' | Out-Null
    Invoke-Msi "/i `"$($fixtureMsi['1.1.0'])`" REINSTALL=ALL REINSTALLMODE=vomus" 0 'reinstall 1.1.0' | Out-Null
    Assert-Installed $custom $fx.'1.1.0'.files $false $extra '1.1.0 after repair and reinstall'

    # 4. Downgrade is refused and changes nothing.
    Invoke-Msi "/i `"$($fixtureMsi['1.0.0'])`"" 1603 'downgrade to 1.0.0 is refused' | Out-Null
    Assert-Single $uc $pc['1.1.0'] '1.1.0 still installed after refused downgrade'

    # 5. Upgrade to a dev build without the shortcut; the runner stays off.
    Invoke-Msi "/i `"$($fixtureMsi['1.2.0-dev.aaa'])`" STARTMENUSHORTCUT=0" 0 'upgrade to 1.2.0-dev.aaa with STARTMENUSHORTCUT=0' | Out-Null
    Assert-Installed $custom $fx.'1.2.0-dev.aaa'.files $false $extra '1.2.0-dev.aaa'
    Check (-not (Test-Path -LiteralPath $lnk)) 'STARTMENUSHORTCUT=0 removes the shortcut'
    Pass 'no shortcut with STARTMENUSHORTCUT=0'

    # 6. Same Windows version, different build: refused, then replaced only when asked.
    Invoke-Msi "/i `"$($fixtureMsi['1.2.0-dev.bbb'])`"" 1603 'same-version different build is refused' | Out-Null
    Assert-Single $uc $pc['1.2.0-dev.aaa'] 'dev.aaa still installed'
    Invoke-Msi "/i `"$($fixtureMsi['1.2.0-dev.bbb'])`" METRO_REPLACE_SAME_VERSION=1 LOCALRUNNER=1" 0 'explicit METRO_REPLACE_SAME_VERSION=1' | Out-Null
    Assert-Installed $custom $fx.'1.2.0-dev.bbb'.files $true $extra '1.2.0-dev.bbb with LOCALRUNNER=1'
    Assert-Single $uc $pc['1.2.0-dev.bbb'] 'dev.bbb replaced dev.aaa'
    Check (-not (Test-Path -LiteralPath $lnk)) 'shortcut choice (off) remembered'

    # 7. Uninstall: only Setup's files go; unrelated files and data stay.
    Invoke-Msi "/x $($pc['1.2.0-dev.bbb'])" 0 'uninstall' | Out-Null
    Check ((Get-RelatedProducts $Installer $uc).Count -eq 0) 'no fixture product left'
    $left = Get-Tree $custom
    Check ((Get-SetDifference $extra $left) -eq '') "uninstall left exactly the unrelated files (got $($left -join ', '))"
    # Windows Installer may leave the emptied key itself; no value may remain.
    $values = @(Get-ChildItem -Path $regKey -Recurse -ErrorAction SilentlyContinue | ForEach-Object { $_.GetValueNames() }) +
              @(if (Test-Path $regKey) { (Get-Item -Path $regKey).GetValueNames() })
    Check ($values.Count -eq 0) "installer registry values removed (left: $($values -join ', '))"
    Check ((Get-Content -LiteralPath (Join-Path $data 'keep.txt')) -eq 'keep') 'data kept on uninstall'
    Pass 'uninstall kept data and unrelated files only'

    # 8. A legacy install-metro-windows.ps1 install blocks the MSI.
    New-Item -Path $legacyKey -Force | Out-Null
    Set-ItemProperty -Path $legacyKey -Name InstallLocation -Value (Join-Path $Work 'legacy')
    try { Invoke-Msi "/i `"$($fixtureMsi['1.0.0'])`"" 1603 'refused while a script-managed install is registered' | Out-Null }
    finally { Remove-Item -Path $legacyKey -Recurse -Force }
    Check ((Get-RelatedProducts $Installer $uc).Count -eq 0) 'nothing installed next to the script-managed copy'

    # 9. Default folder: %LOCALAPPDATA%\Programs\<default>.
    $default = Join-Path $LocalAppData ('Programs\' + [string] $Fixtures.default_dir)
    Check (-not (Test-Path -LiteralPath $default)) 'default fixture folder must not exist before the test'
    Invoke-Msi "/i `"$($fixtureMsi['1.0.0'])`"" 0 'install into the default folder' | Out-Null
    Assert-Installed $default $fx.'1.0.0'.files $true @() 'default folder'
    Invoke-Msi "/x $($pc['1.0.0'])" 0 'uninstall from the default folder' | Out-Null
    Check (-not (Test-Path -LiteralPath $default)) 'empty default folder removed'
    Pass 'default folder removed when empty'

    # 10. Another drive, when the machine has a writable D:.
    if ((Test-Path 'D:\') -and ((Get-PSDrive -Name D -ErrorAction SilentlyContinue).Free -gt 100MB)) {
        $dDir = 'D:\Metro MSI Test ' + [Guid]::NewGuid().ToString('N').Substring(0, 6) + '\Program Folder'
        Invoke-Msi "/i `"$($fixtureMsi['1.1.0'])`" INSTALLDIR=`"$dDir`" STARTMENUSHORTCUT=0" 0 'install on drive D:' | Out-Null
        Assert-Installed $dDir $fx.'1.1.0'.files $true @() 'drive D: folder'
        Invoke-Msi "/x $($pc['1.1.0'])" 0 'uninstall from drive D:' | Out-Null
        Check (-not (Test-Path -LiteralPath $dDir)) 'D: folder removed'
        Remove-Item -LiteralPath (Split-Path -Parent $dDir) -Recurse -Force -ErrorAction SilentlyContinue
    } else { Skip 'install on another drive (no writable D:)' }

    # 11. Without administrator rights: a genuine standard local account installs,
    # inspects and uninstalls in its own context. Under this machine's installer
    # policy the outcome must be the documented one (Windows Server blocks
    # non-elevated per-user installs: 1625); where it is blocked, the same account
    # must then succeed with DisableMsi=0 (Windows client behaviour), restored after.
    # runas /trustlevel is not used: on the hosted runner its token kept High
    # integrity. Accounts and policy changes happen only on GitHub-hosted runners.
    $isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    $envMap = @{ GITHUB_ACTIONS = $env:GITHUB_ACTIONS; CI = $env:CI; RUNNER_ENVIRONMENT = $env:RUNNER_ENVIRONMENT }
    $refusal = if ($StandardUserFixture) { Get-PolicyFixtureRefusal $envMap $isAdmin } else { 'run without -StandardUserFixture' }
    Check ($refusal -eq '') "no non-admin install could be verified: $refusal (the fixture runs only on GitHub-hosted runners)"
    $policyStore = New-HklmStore
    $policy = Get-RegistryValueState $policyStore $script:MsiPolicyKey $script:MsiPolicyValue
    $isServer = [int] (Get-CimInstance -ClassName Win32_OperatingSystem).ProductType -ne 1
    $expectation = Get-BasicUserPolicyExpectation $policy $isServer
    Write-Host "Installer policy: DisableMsi $(Format-RegistryValueState $policy); $(if ($isServer) { 'Windows Server' } else { 'Windows client' }); non-admin per-user install expected: $expectation"
    $spec = [ordered] @{
        package = [string] $fx.'1.0.0'.file; product_code = $pc['1.0.0']; upgrade_code = $uc
        reg_key = [string] $Fixtures.reg_key; shortcut = [string] $Fixtures.shortcut; files = $fx.'1.0.0'.files
    }
    Invoke-StandardUserMsiFixture $fixtureMsi['1.0.0'] $spec $expectation $policyStore
    Check ((Get-RelatedProducts $Installer $uc).Count -eq 0) 'the standard-user install left nothing registered for this account either'

    # Part 2: the real MSI.
    if ($Msi) {
        if (-not $Version) { throw '-Version is required with -Msi' }
        # Expectations come from the package itself (as build_msi.py wrote it) and are
        # checked against the release identity and the file name's preview marking.
        $package = Get-MsiPackageFacts $Installer $Msi
        $isPreview = (Split-Path -Leaf $Msi) -like '*.unsigned-preview.msi'
        $problems = Get-PackageIdentityProblems $package (Split-Path -Leaf $Msi) $Version
        Check ($problems.Count -eq 0) "real MSI identity: $($problems -join '; ')"
        Pass "real MSI identity: $($package.ProductName), $($package.ProductCode), preview $isPreview"
        $realUc = $package.UpgradeCode
        Check ((Get-RelatedProducts $Installer $realUc).Count -eq 0) 'Metro must not be installed before the real-MSI test'
        Check (-not (Test-Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\dev.metro.desktop')) 'no script-managed Metro before the test'
        $realData = Join-Path $LocalAppData 'Metro\Desktop'
        $madeData = -not (Test-Path -LiteralPath $realData)
        New-Item -ItemType Directory -Force -Path $realData | Out-Null
        $sentinel = Join-Path $realData ('msi-test-keep-' + [Guid]::NewGuid().ToString('N') + '.txt')
        Set-Content -LiteralPath $sentinel -Value 'keep' -Encoding ASCII
        try {
            foreach ($variant in @(@{ Args = ''; Marker = $true; Lnk = $true; Name = 'defaults' },
                                   @{ Args = 'LOCALRUNNER=0 STARTMENUSHORTCUT=0'; Marker = $false; Lnk = $false; Name = 'runner and shortcut off' })) {
                $dir = Join-Path $Work ("Real Metro $($variant.Name)\Metro Desktop")
                Invoke-Msi "/i `"$Msi`" INSTALLDIR=`"$dir`" $($variant.Args)" 0 "real MSI $Version ($($variant.Name))" | Out-Null
                $manifest = Get-Content -LiteralPath (Join-Path $dir 'RELEASE.json') -Raw | ConvertFrom-Json
                Check ($manifest.version -eq $Version -and $manifest.target -eq 'windows-x64') 'installed RELEASE.json describes this version'
                Assert-Installed $dir $manifest.files $variant.Marker @() "real MSI ($($variant.Name))"
                foreach ($f in @('Metro.exe', 'metro-server.exe', 'ui/Metro.exe') + $RequireFile) {
                    Check (Test-Path -LiteralPath (Join-Path $dir $f)) "real MSI installs $f"
                }
                $mlnk = Get-Lnk $package.Shortcuts[0]
                Check ((Test-Path -LiteralPath $mlnk) -eq $variant.Lnk) "Start menu shortcut $(if ($variant.Lnk) {'present'} else {'absent'})"
                if ($variant.Lnk) { Check ((Get-LnkTarget $mlnk) -eq (Join-Path $dir 'Metro.exe')) 'real shortcut points into the chosen folder' }
                $rpc = Get-RelatedProducts $Installer $realUc
                Check ($rpc.Count -eq 1 -and $rpc[0] -eq $package.ProductCode) "one Metro product, this package's (got $($rpc -join ', '))"
                $facts = Get-ProductFacts $Installer $rpc[0]
                Check ($facts.AssignmentType -eq '0' -and $facts.InstallLocation.TrimEnd('\') -eq $dir) "real MSI installed per-user into the chosen folder (AssignmentType $($facts.AssignmentType))"
                Set-Content -LiteralPath (Join-Path $dir 'user-notes.txt') -Value 'mine' -Encoding ASCII
                Invoke-Msi "/x $($rpc[0])" 0 "uninstall real MSI ($($variant.Name))" | Out-Null
                $left = Get-Tree $dir
                Check ((Get-SetDifference @('user-notes.txt') $left) -eq '') "real uninstall left only the unrelated file (got $($left -join ', '))"
                Check (-not (Test-Path -LiteralPath $mlnk)) 'real shortcut removed'
                Check ((Get-Content -LiteralPath $sentinel) -eq 'keep') 'Metro data kept'
                Pass "real MSI ($($variant.Name)) uninstall kept Metro data and unrelated files"
            }
        } finally {
            Remove-Item -LiteralPath $sentinel -Force -ErrorAction SilentlyContinue
            if ($madeData) { Remove-Item -LiteralPath $realData -Recurse -Force -ErrorAction SilentlyContinue }
        }
    }
    Write-Host ''
    Write-Host 'All MSI checks passed.'
    $script:Results | ForEach-Object { Write-Host $_ }
} catch {
    $script:Failed = $true
    Write-Host "FAILED $(Format-FailureLocation $_): $($_.Exception.Message)"
    throw
} finally {
    # Cleanup must not hide the failure above; its own error is reported, and
    # raised only when everything before it passed.
    try {
        foreach ($p in (Get-RelatedProducts $Installer ([string] $Fixtures.upgrade_code))) {
            Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" -ArgumentList "/x $p /qn /norestart" -Wait | Out-Null
        }
        Remove-Item -LiteralPath (Join-Path $LocalAppData 'Metro MSI Fixture Data') -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $Work -Recurse -Force -ErrorAction SilentlyContinue
    } catch {
        Write-Host "cleanup FAILED $(Format-FailureLocation $_): $($_.Exception.Message)"
        if (-not $script:Failed) { throw }
    }
    Write-Host "msiexec logs: $LogDir"
}
