<#
.SYNOPSIS
  Metro desktop for Windows x64: per-user install, update and uninstall from a
  prebuilt Metro-<version>-windows-x64.zip. No administrator rights are needed.

.DESCRIPTION
  Installs into %LOCALAPPDATA%\Programs\Metro\<version>, adds a Start menu
  shortcut and a per-user "Installed apps" entry. Your Metro data in
  %LOCALAPPDATA%\Metro\Desktop is never touched. Local runner support (an
  empty "enable-local-runner" file next to Metro.exe) is on for a new install
  unless -NoLocalRunner is given; updates keep the previous choice.

.EXAMPLE
  .\install-metro-windows.ps1 -Action Install -Archive .\Metro-0.2.0-windows-x64.zip

  Use the Authenticode-signed copy from the Metro release, and check it first:
  (Get-AuthenticodeSignature .\install-metro-windows.ps1).Status must be Valid.
  It runs under the RemoteSigned or AllSigned execution policy. Never bypass or
  turn off your execution policy for Metro.
#>
[CmdletBinding()]
param(
    [ValidateSet('Install', 'Update', 'Uninstall', 'Status', 'Verify')]
    [string] $Action = 'Install',
    [string] $Archive,
    [string] $Sha256,
    [string] $ChecksumFile,
    [switch] $NoLocalRunner,
    [switch] $LocalRunner,
    [switch] $NoShortcut,
    [switch] $Quiet
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$Prog = 'install-metro-windows'
$Mark = '.metro-desktop-install'
$UninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\dev.metro.desktop'

function Fail([string] $Message) { throw "${Prog}: $Message" }
function Say([string] $Message) { if (-not $Quiet) { Write-Host $Message } }

$OnWindows = [System.Environment]::OSVersion.Platform -eq 'Win32NT'
$LocalAppData = [Environment]::GetFolderPath('LocalApplicationData')
if ($env:METRO_INSTALL_DIR) { $Root = $env:METRO_INSTALL_DIR } else { $Root = Join-Path $LocalAppData 'Programs\Metro' }
if (-not [System.IO.Path]::IsPathRooted($Root)) { Fail 'METRO_INSTALL_DIR must be an absolute path' }
$UserData = Join-Path $LocalAppData 'Metro\Desktop'

function Get-Sha256([string] $Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }

function Get-ExpectedSum([string] $ArchivePath) {
    $name = Split-Path -Leaf $ArchivePath
    if ($Sha256) { $expected = $Sha256.Trim().ToLowerInvariant(); $source = '-Sha256' }
    else {
        $file = $ChecksumFile
        if (-not $file) {
            $dir = Split-Path -Parent $ArchivePath
            if (Test-Path -LiteralPath "$ArchivePath.sha256" -PathType Leaf) { $file = "$ArchivePath.sha256" }
            elseif (Test-Path -LiteralPath (Join-Path $dir 'SHA256SUMS') -PathType Leaf) { $file = Join-Path $dir 'SHA256SUMS' }
            else { Fail "no checksum for ${name}: pass -Sha256 HEX or -ChecksumFile FILE (the release's $name.sha256 or SHA256SUMS)" }
        }
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { Fail "checksum file not found: $file" }
        $found = @(Get-Content -LiteralPath $file | ForEach-Object {
            $parts = $_ -split '\s+', 2
            if ($parts.Count -eq 2 -and ($parts[1] -eq $name -or $parts[1] -eq "*$name") -and $parts[0] -match '^[0-9a-fA-F]{64}$') { $parts[0].ToLowerInvariant() }
        })
        if ($found.Count -ne 1) { Fail "$file does not list exactly one SHA-256 for $name" }
        $expected = $found[0]; $source = $file
    }
    if ($expected -notmatch '^[0-9a-f]{64}$') { Fail "invalid SHA-256 value from $source" }
    return $expected
}

# Extracts the archive into $Dest after checking every entry, then checks
# every file against RELEASE.json and that nothing else is present.
function Expand-Verified([string] $ArchivePath, [string] $Dest, [string] $Top) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($ArchivePath)
    try {
        foreach ($entry in $zip.Entries) {
            $n = $entry.FullName
            if ($n.Contains('\') -or -not $n.StartsWith("$Top/") -or $n.EndsWith('/') -or
                ($n -split '/') -contains '..' -or ($n -split '/') -contains '.' -or $n.Contains(':')) {
                Fail "unexpected archive entry: $n"
            }
        }
    } finally { $zip.Dispose() }
    [System.IO.Compression.ZipFile]::ExtractToDirectory($ArchivePath, $Dest)
    $dir = Join-Path $Dest $Top
    $manifestPath = Join-Path $dir 'RELEASE.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { Fail 'archive has no RELEASE.json manifest' }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    $want = @{}
    foreach ($p in $manifest.files.PSObject.Properties) {
        $rel = [string] $p.Name; $hash = [string] $p.Value
        if ($rel -match '(^|/)\.\.?(/|$)' -or $rel.StartsWith('/') -or $rel.Contains('\') -or $hash -notmatch '^[0-9a-f]{64}$') {
            Fail "RELEASE.json lists an invalid file: $rel"
        }
        $path = Join-Path $dir ($rel -replace '/', [System.IO.Path]::DirectorySeparatorChar)
        if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or (Get-Sha256 $path) -ne $hash) {
            Fail "files in the archive do not match its RELEASE.json manifest ($rel)"
        }
        $want[$rel] = $true
    }
    if ($want.Count -eq 0) { Fail 'RELEASE.json has no file list' }
    $want['RELEASE.json'] = $true
    $base = (Resolve-Path -LiteralPath $dir).ProviderPath.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
    foreach ($f in Get-ChildItem -LiteralPath $dir -Recurse -Force) {
        if ($f.Attributes -band [System.IO.FileAttributes]::ReparsePoint) { Fail "archive contains a link: $($f.FullName)" }
        if (-not $f.PSIsContainer) {
            $rel = $f.FullName.Substring($base.Length) -replace '\\', '/'
            if (-not $want.ContainsKey($rel)) { Fail "archive has a file that its RELEASE.json does not list: $rel" }
        }
    }
    return @{ Dir = $dir; Manifest = $manifest }
}

function Test-Prerequisites {
    if (-not $OnWindows) { Fail 'this installer is for Windows' }
    if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
        Fail 'Metro for Windows is built for x64 Windows 10 1809 or newer, or Windows 11 x64'
    }
    $sys = [Environment]::GetFolderPath('System')
    $missing = @('vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll') | Where-Object { -not (Test-Path -LiteralPath (Join-Path $sys $_)) }
    if ($missing) {
        Fail ("Microsoft Visual C++ Redistributable (x64) is missing ($($missing -join ', ')). " +
              'Install it from https://aka.ms/vs/17/release/vc_redist.x64.exe and run this again.')
    }
    if (-not (Test-Path -LiteralPath (Join-Path $sys 'mfplat.dll'))) {
        Fail 'Windows Media Foundation is missing. On Windows N editions, install the Media Feature Pack (Settings > Apps > Optional features) and run this again.'
    }
}

function Test-Running {
    $prefix = (Join-Path $Root 'versions') + [System.IO.Path]::DirectorySeparatorChar
    $procs = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) }
    return [bool] $procs
}

function Get-Current {
    $file = Join-Path $Root 'current.txt'
    if (Test-Path -LiteralPath $file -PathType Leaf) {
        $v = (Get-Content -LiteralPath $file -Raw).Trim()
        if ($v -and (Test-Path -LiteralPath (Join-Path $Root "versions\$v\Metro.exe") -PathType Leaf)) { return $v }
    }
    return $null
}

function Get-ShortcutPath { Join-Path ([Environment]::GetFolderPath('Programs')) 'Metro.lnk' }

function Install-Metro {
    if (-not $Archive) { Fail '-Archive is required' }
    if (-not (Test-Path -LiteralPath $Archive -PathType Leaf)) { Fail "archive not found: $Archive" }
    $archivePath = (Resolve-Path -LiteralPath $Archive).ProviderPath
    $name = Split-Path -Leaf $archivePath
    if ($name -notmatch '^Metro-(v?\d+\.\d+\.\d+(?:[-+][A-Za-z0-9][A-Za-z0-9.-]{0,40})?)-windows-x64\.zip$') {
        Fail "expected a Metro-<version>-windows-x64.zip archive, got $name"
    }
    $version = $Matches[1]
    $top = $name.Substring(0, $name.Length - 4)
    if ($Action -eq 'Verify') {
        # Only check the archive: work in a throwaway folder, install nothing.
        $script:Root = Join-Path ([System.IO.Path]::GetTempPath()) ('metro-verify-' + [Guid]::NewGuid().ToString('N'))
    } else {
        Test-Prerequisites
        if ($Action -eq 'Update' -and -not (Get-Current)) { Fail "Metro is not installed in $Root; use -Action Install" }
    }
    $expected = Get-ExpectedSum $archivePath
    $actual = Get-Sha256 $archivePath
    if ($actual -ne $expected) { Fail "checksum mismatch for $name (expected $expected, got $actual). Download it again; do not install it." }
    Say "Checksum OK: $name"

    New-Item -ItemType Directory -Force -Path $Root | Out-Null
    if ((Get-ChildItem -LiteralPath $Root -Force | Measure-Object).Count -gt 0 -and -not (Test-Path -LiteralPath (Join-Path $Root $Mark))) {
        Fail "$Root exists but was not created by this installer; refusing to change it"
    }
    $lock = Join-Path $Root '.lock'
    try { New-Item -ItemType Directory -Path $lock -ErrorAction Stop | Out-Null }
    catch { Fail "another install is running (or a previous one crashed: remove $lock and retry)" }
    $stage = Join-Path $Root ('.stage.' + [Guid]::NewGuid().ToString('N'))
    try {
        Set-Content -LiteralPath (Join-Path $Root $Mark) -Value "Metro desktop per-user install (managed by $Prog). Your data is in $UserData." -Encoding UTF8
        New-Item -ItemType Directory -Path $stage | Out-Null
        $result = Expand-Verified $archivePath $stage $top
        $m = $result.Manifest
        if ($m.kind -ne 'desktop' -or $m.target -ne 'windows-x64' -or $m.version -ne $version) {
            Fail "RELEASE.json does not describe a Metro $version windows-x64 desktop package"
        }
        foreach ($f in 'Metro.exe', 'metro-server.exe', 'ui\Metro.exe') {
            if (-not (Test-Path -LiteralPath (Join-Path $result.Dir $f) -PathType Leaf)) { Fail "package is missing $f" }
        }
        if ($Action -eq 'Verify') { Say "Archive verified: $name"; return }
        if (Test-Running) { Fail 'Metro is running from this install; close it first' }

        $previous = Get-Current
        $enable = $true
        if ($NoLocalRunner) { $enable = $false }
        elseif ($LocalRunner) { $enable = $true }
        elseif ($previous) { $enable = Test-Path -LiteralPath (Join-Path $Root "versions\$previous\enable-local-runner") -PathType Leaf }
        if ($enable) { [System.IO.File]::WriteAllBytes((Join-Path $result.Dir 'enable-local-runner'), [byte[]]@()) }

        $versions = Join-Path $Root 'versions'
        New-Item -ItemType Directory -Force -Path $versions | Out-Null
        $target = Join-Path $versions $version
        $old = $null
        if (Test-Path -LiteralPath $target) {
            $old = Join-Path $Root ('.old.' + [Guid]::NewGuid().ToString('N'))
            Move-Item -LiteralPath $target -Destination $old
        }
        Move-Item -LiteralPath $result.Dir -Destination $target
        $tmp = Join-Path $Root 'current.txt.new'
        Set-Content -LiteralPath $tmp -Value $version -Encoding ASCII
        Move-Item -LiteralPath $tmp -Destination (Join-Path $Root 'current.txt') -Force
        foreach ($d in Get-ChildItem -LiteralPath $versions -Directory) {
            if ($d.Name -ne $version) { Remove-Item -LiteralPath $d.FullName -Recurse -Force }
        }
        if ($old) { Remove-Item -LiteralPath $old -Recurse -Force }

        # Keep a copy of this script for the "Installed apps" uninstall entry.
        $self = Join-Path $Root 'install-metro-windows.ps1'
        if ($PSCommandPath -and ((Resolve-Path -LiteralPath $PSCommandPath).ProviderPath -ne $self)) {
            Copy-Item -LiteralPath $PSCommandPath -Destination $self -Force
        }
        $exe = Join-Path $target 'Metro.exe'
        if (-not $NoShortcut) {
            $shell = New-Object -ComObject WScript.Shell
            $lnk = $shell.CreateShortcut((Get-ShortcutPath))
            $lnk.TargetPath = $exe
            $lnk.WorkingDirectory = $target
            $lnk.IconLocation = "$exe,0"
            $lnk.Description = 'Metro'
            $lnk.Save()
        }
        New-Item -Path $UninstallKey -Force | Out-Null
        $ps = Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe'
        $values = @{
            DisplayName = 'Metro'; DisplayVersion = $version; Publisher = 'Metro'; InstallLocation = $Root
            DisplayIcon = $exe
            # RemoteSigned still requires this copy's release signature when it carries
            # a download mark; Group Policy, if set, overrides it. No Bypass.
            UninstallString = "`"$ps`" -NoProfile -ExecutionPolicy RemoteSigned -File `"$self`" -Action Uninstall"
        }
        foreach ($k in $values.Keys) { Set-ItemProperty -Path $UninstallKey -Name $k -Value $values[$k] }
        Set-ItemProperty -Path $UninstallKey -Name NoModify -Value 1 -Type DWord
        Set-ItemProperty -Path $UninstallKey -Name NoRepair -Value 1 -Type DWord

        Say "Metro $version is installed in $target"
        $supported = ($m.PSObject.Properties.Name -contains 'local_runner') -and $m.local_runner -eq 'installer-marker'
        if (-not $enable) { Say 'Local runner support is off (install again with -LocalRunner to turn it on).' }
        elseif ($supported) {
            Say 'Local runner support is on. Metro sets up your personal on-device runner after you sign in; it is not attached to any Space until you choose it under Manage Space.'
        } else {
            Say 'Local runner support was requested, but this Metro build predates it, so no runner will be set up. It takes effect once you update to a build that supports it.'
        }
        Say "Your Metro data stays in $UserData."
    } finally {
        if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue }
        Remove-Item -LiteralPath $lock -Force -ErrorAction SilentlyContinue
        if ($Action -eq 'Verify') { Remove-Item -LiteralPath $Root -Recurse -Force -ErrorAction SilentlyContinue }
    }
}

function Uninstall-Metro {
    if (-not $OnWindows) { Fail 'this installer is for Windows' }
    if (Test-Path -LiteralPath $Root) {
        if (-not (Test-Path -LiteralPath (Join-Path $Root $Mark))) { Fail "$Root was not created by this installer; refusing to remove it" }
        if (Test-Running) { Fail 'Metro is running; close it first' }
    }
    $lnk = Get-ShortcutPath
    if (Test-Path -LiteralPath $lnk) { Remove-Item -LiteralPath $lnk -Force }
    if (Test-Path -LiteralPath $UninstallKey) { Remove-Item -LiteralPath $UninstallKey -Recurse -Force }
    if (Test-Path -LiteralPath $Root) {
        # The uninstall entry may run the copy of this script inside $Root.
        Get-ChildItem -LiteralPath $Root -Force | Where-Object { $_.Name -ne 'install-metro-windows.ps1' } |
            Remove-Item -Recurse -Force
        $self = Join-Path $Root 'install-metro-windows.ps1'
        if ($PSCommandPath -and (Test-Path -LiteralPath $self) -and ((Resolve-Path -LiteralPath $PSCommandPath).ProviderPath -eq $self)) {
            # Delete the running script and its folder after this process exits.
            Start-Process -WindowStyle Hidden -FilePath "$env:ComSpec" -ArgumentList "/d /c ping -n 3 127.0.0.1 >nul & rd /s /q `"$Root`""
        } else { Remove-Item -LiteralPath $Root -Recurse -Force }
    }
    Say "Metro was removed. Your Metro data was kept in $UserData (delete that folder yourself to remove it)."
}

try {
    switch ($Action) {
        'Install' { Install-Metro }
        'Update' { Install-Metro }
        'Verify' { Install-Metro }
        'Uninstall' { Uninstall-Metro }
        'Status' {
            $v = Get-Current
            if ($v) {
                Say "Installed: Metro $v in $(Join-Path $Root "versions\$v")"
                $m = Get-Content -LiteralPath (Join-Path $Root "versions\$v\RELEASE.json") -Raw | ConvertFrom-Json
                $supported = ($m.PSObject.Properties.Name -contains 'local_runner') -and $m.local_runner -eq 'installer-marker'
                if (-not (Test-Path -LiteralPath (Join-Path $Root "versions\$v\enable-local-runner"))) { Say 'Local runner support: off' }
                elseif ($supported) { Say 'Local runner support: on' }
                else { Say 'Local runner support: requested (this build predates it; no runner is set up)' }
            } else { Say "Metro is not installed in $Root." }
            Say "Data: $UserData"
        }
    }
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
