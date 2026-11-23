<#
  Runs as the ephemeral standard (non-admin) account that test-msi-windows.ps1
  creates on a GitHub-hosted runner. Everything here happens in that user's own
  context: its token, its per-user Windows Installer registration, its HKCU and
  its Start menu. It never elevates and never sees a credential.

  -Expect refused : one install attempt that the machine policy must refuse (1625)
  -Expect allowed : install, inspect, uninstall

  Results go to <Root>\out\<Phase>.json (read by the parent); msiexec logs next to it.
#>
param(
    [Parameter(Mandatory = $true)] [string] $Root,
    [Parameter(Mandatory = $true)] [ValidateSet('host', 'client')] [string] $Phase,
    [Parameter(Mandatory = $true)] [ValidateSet('refused', 'allowed')] [string] $Expect
)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'MsiAutomation.ps1')
. (Join-Path $PSScriptRoot 'MsiHarness.ps1')

$out = Join-Path $Root 'out'
$result = [ordered] @{ phase = $Phase; expect = $Expect; user_sid = ''; checks = [Collections.Generic.List[object]]::new(); error = '' }
function Record([string] $Name, [bool] $Ok, [string] $Detail = '') {
    $result.checks.Add([ordered] @{ name = $Name; ok = $Ok; detail = $Detail })
    if (-not $Ok) { throw "check '$Name' failed: $Detail" }
}
function Invoke-Msiexec([string] $Arguments, [string] $LogName) {
    $log = Join-Path $out $LogName
    $p = Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" -ArgumentList "$Arguments /qn /norestart /l*v `"$log`"" -PassThru
    $null = $p.Handle
    if (-not $p.WaitForExit(600000)) { $p.Kill(); throw "msiexec timed out ($LogName)" }
    return $p.ExitCode
}

try {
    $spec = Get-Content -LiteralPath (Join-Path $Root 'spec.json') -Raw | ConvertFrom-Json
    $package = Join-Path $Root ([string] $spec.package)
    $result.user_sid = ([string] (& "$env:SystemRoot\System32\whoami.exe" /user /fo csv /nh) | ConvertFrom-Csv -Header Name, Sid).Sid
    $groups = @(& "$env:SystemRoot\System32\whoami.exe" /groups /fo csv /nh)
    $tokenProblem = Get-NonAdminTokenProblem $groups
    $isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    Record 'token is standard' ($tokenProblem -eq '' -and -not $isAdmin) "$tokenProblem$(if ($isAdmin) { ' (in the Administrators role)' })"

    $installer = New-Object -ComObject WindowsInstaller.Installer
    $uc = [string] $spec.upgrade_code
    Record 'no product before' ((Get-RelatedProducts $installer $uc).Count -eq 0)
    $localAppData = [Environment]::GetFolderPath('LocalApplicationData')
    $dir = Join-Path $localAppData 'Metro MSI Standard User\Chosen Folder'
    $code = Invoke-Msiexec "/i `"$package`" INSTALLDIR=`"$dir`"" "$Phase-install-msiexec.log"
    if ($Expect -eq 'refused') {
        Record 'msiexec refused by policy (1625)' ($code -eq 1625) "exit $code"
        Record 'nothing registered' ((Get-RelatedProducts $installer $uc).Count -eq 0)
        Record 'nothing installed' (-not (Test-Path -LiteralPath $dir) -or (Get-Tree $dir).Count -eq 0)
    } else {
        Record 'msiexec installed (0)' ($code -eq 0) "exit $code"
        $related = Get-RelatedProducts $installer $uc
        Record 'one per-user product' ($related.Count -eq 1 -and $related[0] -eq [string] $spec.product_code) ($related -join ', ')
        $facts = Get-ProductFacts $installer $related[0]
        Record 'install location' ($facts.AssignmentType -eq '0' -and $facts.InstallLocation.TrimEnd('\') -eq $dir) "AssignmentType $($facts.AssignmentType)"
        $problems = Get-InstalledTreeProblems $dir $spec.files $true @()
        Record 'installed files' ($problems.Count -eq 0) ($problems -join '; ')
        $remembered = (Get-ItemProperty -LiteralPath ('HKCU:\' + [string] $spec.reg_key) -Name InstallDir -ErrorAction SilentlyContinue)
        Record 'remembered folder in own HKCU' ($remembered -and ([string] $remembered.InstallDir).TrimEnd('\') -eq $dir)
        $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) ([string] $spec.shortcut)
        $target = if (Test-Path -LiteralPath $lnk) { (New-Object -ComObject WScript.Shell).CreateShortcut($lnk).TargetPath } else { '' }
        Record 'own Start menu shortcut' ($target -eq (Join-Path $dir 'Metro.exe'))
        $code = Invoke-Msiexec "/x $($related[0])" "$Phase-uninstall-msiexec.log"
        Record 'msiexec uninstalled (0)' ($code -eq 0) "exit $code"
        Record 'product removed' ((Get-RelatedProducts $installer $uc).Count -eq 0)
        Record 'folder removed' (-not (Test-Path -LiteralPath $dir))
        Record 'shortcut removed' (-not (Test-Path -LiteralPath $lnk))
    }
} catch {
    $result.error = "$(Format-FailureLocation $_): $($_.Exception.Message)"
} finally {
    $json = $result | ConvertTo-Json -Depth 5
    [IO.File]::WriteAllText((Join-Path $out "$Phase.json"), $json, [Text.UTF8Encoding]::new($false))
}
if ($result.error) { exit 1 }
exit 0
