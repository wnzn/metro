<#
.SYNOPSIS
  Native Windows checks for Metro's in-app MSI update. CI only: it installs
  and removes per-user Metro products in the current Windows profile (and,
  with -StandardUser, in an ephemeral standard account's).

.DESCRIPTION
  Inputs are test builds from update_fixture.py: MSI A and MSI B (unsigned
  previews built by build_msi.py from payloads whose Metro.exe is an
  `e2e-test-release` build of versions A and B, with the stand-in UI), and
  the fixture release of B (`update_fixture.py release`). This script serves
  that release on 127.0.0.1 (`update_fixture.py serve`), points the builds at
  it with METRO_TEST_RELEASE_BASE, and runs msi-update-scenarios.ps1:
    update, failure-retry, helper-killed, helper-msiexec-killed
  as the current user, then (with -StandardUser, GitHub-hosted runners only)
  `update` and `failure-retry` as an ephemeral standard account, with the
  machine's per-user install policy handled as in test-msi-windows.ps1.

.EXAMPLE
  pwsh -File test-msi-update.ps1 -MsiA A.msi -MsiB B.msi -VersionA 1.0.0 -VersionB 1.0.1 -ReleaseRoot .\release -StandardUser
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $MsiA,
    [Parameter(Mandatory = $true)] [string] $MsiB,
    [Parameter(Mandatory = $true)] [string] $VersionA,
    [Parameter(Mandatory = $true)] [string] $VersionB,
    [Parameter(Mandatory = $true)] [string] $ReleaseRoot,
    [string[]] $Scenarios = @('update', 'failure-retry', 'helper-killed', 'helper-msiexec-killed'),
    [string] $Python = 'python',
    [string] $LogDir,
    [switch] $StandardUser,
    [switch] $IAcceptChangesToThisWindowsProfile
)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
if ([System.Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'test-msi-update.ps1 runs on Windows only' }
if (-not ($env:CI -eq 'true' -or $IAcceptChangesToThisWindowsProfile)) {
    throw 'This test installs and removes per-user products in this Windows profile. Run it in CI (CI=true) or pass -IAcceptChangesToThisWindowsProfile.'
}
. (Join-Path $PSScriptRoot 'MsiAutomation.ps1')
. (Join-Path $PSScriptRoot 'MsiHarness.ps1')

$UpgradeCode = '{29BBE899-7D6A-49A7-B60E-F9644544D683}'
if (-not $LogDir) { $LogDir = Join-Path ([IO.Path]::GetTempPath()) ('metro-msi-update-' + [Guid]::NewGuid().ToString('N')) }
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
foreach ($f in $MsiA, $MsiB) { if (-not (Test-Path -LiteralPath $f -PathType Leaf)) { throw "missing $f" } }
if (-not (Test-Path -LiteralPath (Join-Path $ReleaseRoot "v$VersionB\SHA256SUMS"))) { throw "no fixture release v$VersionB under $ReleaseRoot" }
$Failed = $false
$Scripts = 'msi-update-scenarios.ps1', 'MsiAutomation.ps1', 'MsiHarness.ps1'

function New-TestRoot([string] $Base, [string] $ReleaseBase) {
    New-Item -ItemType Directory -Path $Base | Out-Null   # fails if it already exists
    New-Item -ItemType Directory -Path (Join-Path $Base 'out') | Out-Null
    foreach ($f in $Scripts) { Copy-Item -LiteralPath (Join-Path $PSScriptRoot $f) -Destination $Base }
    Copy-Item -LiteralPath $MsiA -Destination (Join-Path $Base 'a.msi')
    Copy-Item -LiteralPath $MsiB -Destination (Join-Path $Base 'b.msi')
    $spec = [ordered] @{ msi_a = 'a.msi'; msi_b = 'b.msi'; version_a = $VersionA; version_b = $VersionB
                         release_base = $ReleaseBase; upgrade_code = $UpgradeCode }
    [IO.File]::WriteAllText((Join-Path $Base 'spec.json'), ($spec | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
}

function Show-Result([string] $File, [string] $Who) {
    if (-not (Test-Path -LiteralPath $File)) { Write-Host "FAIL  $Who wrote no result"; return $false }
    $r = Get-Content -LiteralPath $File -Raw | ConvertFrom-Json
    foreach ($c in $r.checks) { Write-Host ("{0}  {1}: {2}{3}" -f $(if ($c.ok) { 'PASS' } else { 'FAIL' }), $Who, $c.name, $(if ($c.detail) { " ($($c.detail))" })) }
    foreach ($s in $r.scenarios) { Write-Host "INFO  $Who`: $($s.scenario) -> $($s.outcome)" }
    if ($r.error) { Write-Host "FAIL  $Who`: $($r.error)"; return $false }
    return $true
}

$server = $null
try {
    # The fixture release, on loopback only.
    $portFile = Join-Path $LogDir 'release-port.txt'
    $server = Start-Process -FilePath $Python -PassThru -WindowStyle Hidden -ArgumentList @(
        '-I', "`"$(Join-Path $PSScriptRoot 'update_fixture.py')`"", 'serve', '--root', "`"$ReleaseRoot`"",
        '--latest', $VersionB, '--port-file', "`"$portFile`"")
    $deadline = (Get-Date).AddSeconds(30)
    while (-not (Test-Path -LiteralPath $portFile) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
    if (-not (Test-Path -LiteralPath $portFile)) { throw 'the fixture release server did not start' }
    $base = "http://127.0.0.1:$((Get-Content -LiteralPath $portFile -Raw).Trim())"
    Write-Host "fixture release v$VersionB at $base"

    # 1. The current user.
    $root = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) ('Metro MSI Update ' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    New-TestRoot $root $base
    $p = Start-Process -FilePath (Join-Path $PSHOME 'pwsh.exe') -PassThru -WindowStyle Hidden -ArgumentList @(
        '-NoProfile', '-NonInteractive', '-File', "`"$(Join-Path $root 'msi-update-scenarios.ps1')`"",
        '-Root', "`"$root`"", '-Name', 'current', '-Scenarios', ($Scenarios -join ','))
    $null = $p.Handle
    if (-not $p.WaitForExit(3600000)) { $p.Kill(); throw 'the scenarios timed out' }
    if (-not (Show-Result (Join-Path $root 'out\current.json') 'current user')) { $Failed = $true }
    Copy-Item -Path (Join-Path $root 'out\*') -Destination $LogDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue

    # 2. An ephemeral standard account (GitHub-hosted runners only).
    if ($StandardUser) {
        $envMap = @{}
        foreach ($k in 'GITHUB_ACTIONS', 'CI', 'RUNNER_ENVIRONMENT') { $envMap[$k] = [Environment]::GetEnvironmentVariable($k) }
        $isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
        $refusal = Get-PolicyFixtureRefusal $envMap $isAdmin
        if ($refusal) { Write-Host "SKIP  standard user: $refusal" }
        else {
            $store = New-HklmStore
            $isServer = (Get-CimInstance -ClassName Win32_OperatingSystem).ProductType -ne 1
            $expectation = Get-BasicUserPolicyExpectation (Get-RegistryValueState $store $script:MsiPolicyKey $script:MsiPolicyValue) $isServer
            $name = 'm' + [Guid]::NewGuid().ToString('N').Substring(0, 7)
            $sroot = Join-Path $env:SystemDrive ('MetroMsiUpd-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
            $sid = $null; $password = $null; $created = $false
            try {
                $password = New-EphemeralPassword
                $user = New-LocalUser -Name $name -Password $password -AccountNeverExpires -PasswordNeverExpires `
                    -UserMayNotChangePassword -Description 'Metro MSI update CI test account (ephemeral)'
                $sid = $user.SID.Value
                if (-not @(Get-LocalGroupMember -SID 'S-1-5-32-545' | Where-Object { $_.SID.Value -eq $sid }).Count) {
                    Add-LocalGroupMember -SID 'S-1-5-32-545' -Member $sid
                }
                if (@(Get-LocalGroupMember -SID 'S-1-5-32-544' | Where-Object { $_.SID.Value -eq $sid }).Count) { throw 'the test account is an administrator' }
                New-TestRoot $sroot $base
                $created = $true
                $acl = Get-Acl -LiteralPath $sroot
                $acl.SetAccessRuleProtection($true, $false)
                foreach ($rule in @($acl.Access)) { $null = $acl.RemoveAccessRuleSpecific($rule) }
                $inherit = [Security.AccessControl.InheritanceFlags] 'ContainerInherit, ObjectInherit'
                $none = [Security.AccessControl.PropagationFlags]::None
                foreach ($admin in 'S-1-5-18', 'S-1-5-32-544') {
                    $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new($admin), 'FullControl', $inherit, $none, 'Allow'))
                }
                $userSid = [Security.Principal.SecurityIdentifier]::new($sid)
                $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($userSid, 'ReadAndExecute', $inherit, $none, 'Allow'))
                Set-Acl -LiteralPath $sroot -AclObject $acl
                $outAcl = Get-Acl -LiteralPath (Join-Path $sroot 'out')
                $outAcl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($userSid, 'Modify', $inherit, $none, 'Allow'))
                Set-Acl -LiteralPath (Join-Path $sroot 'out') -AclObject $outAcl
                $cred = [pscredential]::new("$env:COMPUTERNAME\$name", $password)
                $run = {
                    $sp = Start-Process -FilePath (Join-Path $PSHOME 'pwsh.exe') -Credential $cred -LoadUserProfile -WorkingDirectory $sroot -WindowStyle Hidden -PassThru `
                        -ArgumentList "-NoProfile -NonInteractive -File `"$(Join-Path $sroot 'msi-update-scenarios.ps1')`" -Root `"$sroot`" -Name standard -Scenarios update,failure-retry -OwnEnvironment"
                    $null = $sp.Handle
                    if (-not $sp.WaitForExit(3600000)) { $sp.Kill(); throw 'the standard-user scenarios timed out' }
                }
                if ($expectation -eq 'refused') {
                    Write-Host 'INFO  this machine refuses non-elevated per-user installs; running with DisableMsi=0 (client policy)'
                    Invoke-WithClientMsiPolicy $store $run
                } else { & $run }
                if (-not (Show-Result (Join-Path $sroot 'out\standard.json') 'standard user')) { $Failed = $true }
            } finally {
                if ($created) { Copy-Item -Path (Join-Path $sroot 'out\*') -Destination $LogDir -Recurse -Force -ErrorAction SilentlyContinue }
                if ($password) { $password.Dispose() }
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
                    if (Get-LocalUser -SID $sid -ErrorAction SilentlyContinue) { Remove-LocalUser -SID $sid }
                }
                if ($created -and (Test-Path -LiteralPath $sroot)) { Remove-Item -LiteralPath $sroot -Recurse -Force -ErrorAction SilentlyContinue }
            }
        }
    }
} catch {
    Write-Host "FAIL  $($_.Exception.Message)"
    $Failed = $true
} finally {
    if ($server -and -not $server.HasExited) { $server | Stop-Process -Force -ErrorAction SilentlyContinue }
}
Write-Host "logs: $LogDir"
if ($Failed) { exit 1 }
exit 0
