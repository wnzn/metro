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

# The Metro Windows Installer package (packaging/windows-msi) records its
# install in this HKCU key and registers this UpgradeCode. This script never
# installs a second copy next to it and never changes its shortcut or files.
$MsiKey = 'HKCU:\Software\Metro\Desktop MSI'
$MsiUpgradeCode = '{29BBE899-7D6A-49A7-B60E-F9644544D683}'

# Describes an MSI-managed Metro for this user, or returns nothing.
function Get-MsiManagedMetro {
    if (Test-Path -LiteralPath $MsiKey) {
        $p = (Get-ItemProperty -LiteralPath $MsiKey).PSObject.Properties['InstallDir']
        if ($p -and $p.Value) { return "in $($p.Value)" }
        return "(registered in $MsiKey)"
    }
    try {
        $installer = New-Object -ComObject WindowsInstaller.Installer
        $related = $installer.GetType().InvokeMember('RelatedProducts', [Reflection.BindingFlags]::GetProperty, $null, $installer, @($MsiUpgradeCode))
        $count = [int] $related.GetType().InvokeMember('Count', [Reflection.BindingFlags]::GetProperty, $null, $related, $null)
        if ($count -gt 0) { return "(Windows Installer product registered)" }
    } catch { Fail 'cannot ask Windows Installer whether the Metro MSI is installed' }
}

# True only for a shortcut target inside this script's own versions folder.
function Test-OwnShortcut([string] $Target, [string] $InstallRoot) {
    if (-not $Target) { return $false }
    $own = [System.IO.Path]::GetFullPath((Join-Path $InstallRoot 'versions')).TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
    return [System.IO.Path]::GetFullPath($Target).StartsWith($own, [StringComparison]::OrdinalIgnoreCase)
}

function Get-ShortcutTarget([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $null }
    return (New-Object -ComObject WScript.Shell).CreateShortcut($Path).TargetPath
}

# The Metro app updates itself too: it journals each step in
# $Root\.update.json and takes the same kernel lock as this installer.
function Assert-NoInAppUpdate {
    $journal = Join-Path $Root '.update.json'
    $item = Get-Item -LiteralPath $journal -Force -ErrorAction SilentlyContinue
    if (-not $item) { return }
    try {
        if ($item.PSIsContainer -or $item.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint) -or $item.Length -gt 65536) { throw 'unsafe journal' }
        $raw = Get-Content -LiteralPath $journal -Raw
        $tokens = @([regex]::Matches($raw, '"(?:\\.|[^"\\])*"|[{},:\[\]]') | ForEach-Object { $_.Value })
        $known = @('version', 'id', 'from', 'to', 'phase', 'outcome', 'detail', 'local_db_snapshot', 'watchdog_pid', 'reported', 'started_at')
        $found = @()
        for ($i = 0; $i -lt ($tokens.Count - 1); $i++) {
            if ($tokens[$i + 1] -eq ':') {
                $key = $tokens[$i].Trim('"')
                if ($key -cnotin $known -or $key -cin $found) { throw 'invalid or repeated field' }
                $found += $key
            }
        }
        $j = $raw | ConvertFrom-Json
        if (($j.version -isnot [long] -and $j.version -isnot [int]) -or $j.version -ne 1 -or
            $j.id -cnotmatch '^[0-9a-f]{16}$' -or
            ($j.started_at -isnot [long] -and $j.started_at -isnot [int])) { throw 'bad journal header' }
        function Get-JournalVersion([string] $Value) {
            if ($Value -notmatch '^(0|[1-9][0-9]{0,9})\.(0|[1-9][0-9]{0,9})\.(0|[1-9][0-9]{0,9})$') { throw 'bad journal version' }
            return (([uint32]$Matches[1], [uint32]$Matches[2], [uint32]$Matches[3]) | ForEach-Object { '{0:D10}' -f $_ }) -join '.'
        }
        if ((Get-JournalVersion $j.from) -cge (Get-JournalVersion $j.to)) { throw 'downwards transition' }
        $outcome = if ('outcome' -cin $found) { $j.outcome }
        $pidValue = if ('watchdog_pid' -cin $found) { $j.watchdog_pid }
        foreach ($field in @('reported', 'local_db_snapshot')) {
            if ($field -cin $found -and $j.$field -isnot [bool]) { throw 'bad flag' }
        }
        if ($null -ne $pidValue -and (($pidValue -isnot [long] -and $pidValue -isnot [int]) -or $pidValue -le 0 -or $pidValue -ge 4294967296)) { throw 'bad pid' }
        if ($j.phase -ceq 'staged' -and ($null -ne $pidValue -or ('local_db_snapshot' -cin $found -and $j.local_db_snapshot))) { throw 'staged trial' }
        $current = Get-Current
        $valid = switch -CaseSensitive ($j.phase) {
            'staged' { $null -eq $outcome -and $current -ceq $j.from }
            'trial' { $null -eq $outcome -and $current -ceq $j.from }
            { $_ -cin @('committing', 'rolling_back') } { $null -eq $outcome -and $current -cin @($j.from, $j.to) }
            'committed' { $outcome -ceq 'updated' -and $current -ceq $j.to }
            'failed' { $outcome -cin @('rolled_back', 'not_confirmed', 'install_failed') -and $current -ceq $j.from }
            default { $false }
        }
        if (-not $valid) { throw 'foreign journal' }
    } catch {
        Fail "$journal has an unknown in-app update state; start Metro once, then run this again"
    }
    if ($j.phase -cin @('trial', 'committing', 'rolling_back')) {
        Fail "Metro is in the middle of updating itself to $($j.to). Let it finish, or close Metro and start it once to complete or undo the update, then run this again"
    }
    if ($j.phase -ceq 'staged') { return $j.to }
}

function Get-LockBusyMessage([string] $Lock) {
    $owner = Join-Path $Lock 'owner'
    if ((Test-Path -LiteralPath $owner -PathType Leaf) -and ((Get-Content -LiteralPath $owner -Raw) -match '"by":"metro-desktop"')) {
        return "Metro is preparing an in-app update right now (it holds $Lock). Wait for it to finish, then run this again"
    }
    return "another install is running (or a previous one crashed: remove $Lock and retry)"
}

function Get-InstallLockPath {
    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    return Join-Path (Split-Path -Parent $rootPath) ('.' + (Split-Path -Leaf $rootPath) + '.install.lock')
}

function Get-LegacyLockToken([string] $Path) {
    $dir = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if (-not $dir -or -not $dir.PSIsContainer -or $dir.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { return }
    $entries = @(Get-ChildItem -LiteralPath $Path -Force)
    if ($entries.Count -ne 1 -or $entries[0].Name -cne 'owner.metro-lock-v2' -or
        $entries[0].PSIsContainer -or $entries[0].Length -gt 128 -or
        $entries[0].Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { return }
    $raw = [IO.File]::ReadAllText($entries[0].FullName)
    if ($raw -cmatch '\Ametro-lock-v2:[A-Za-z0-9._-]+\n\z') { return $raw }
}

function Move-LegacyLockAside([string] $Path) {
    $retired = Join-Path $Root ('.unlock.' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::Move($Path, $retired)
    Remove-Item -LiteralPath (Join-Path $retired 'owner.metro-lock-v2') -Force
    [IO.Directory]::Delete($retired)
}

function Enter-LegacyLock {
    $legacy = Join-Path $Root '.lock'
    if (Get-Item -LiteralPath $legacy -Force -ErrorAction SilentlyContinue) {
        if (-not (Get-LegacyLockToken $legacy)) { Fail (Get-LockBusyMessage $legacy) }
        Move-LegacyLockAside $legacy
    }
    $name = '.lock-new.' + [Guid]::NewGuid().ToString('N')
    $tmp = Join-Path $Root $name
    $token = "metro-lock-v2:$name`n"
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        [IO.File]::WriteAllText((Join-Path $tmp 'owner.metro-lock-v2'), $token)
        # Directory.Move does not replace even an empty legacy lock.
        [IO.Directory]::Move($tmp, $legacy)
        $script:LegacyLockToken = $token
    } finally {
        if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Recurse -Force }
    }
}

function Exit-LegacyLock {
    $legacy = Join-Path $Root '.lock'
    if ((Get-LegacyLockToken $legacy) -ceq $script:LegacyLockToken) { Move-LegacyLockAside $legacy }
}

function Enter-InstallLock {
    $item = Get-Item -LiteralPath $Root -Force
    if (-not $item.PSIsContainer -or $item.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
        Fail 'the install directory is not a plain directory'
    }
    $path = Get-InstallLockPath
    $entry = Get-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
    if ($entry -and ($entry.PSIsContainer -or $entry.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint))) { Fail 'the install lock is not a plain file' }
    $handle = $null
    try {
        # Rust's File::try_lock also locks byte zero. Omitting FileShare.Delete
        # keeps this kernel lock on the same file until the last holder exits.
        $handle = [IO.File]::Open($path, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::ReadWrite)
        $handle.Lock(0, 1)
        Enter-LegacyLock
        return $handle
    } catch {
        if ($handle) { $handle.Dispose() }
        Fail 'another install is running; wait for it to finish'
    }
}

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
        $msi = @(Get-MsiManagedMetro)
        if ($msi.Count -gt 0) {
            Fail ("Metro is already installed by the Metro Windows Installer package (MSI) $($msi -join ' '). " +
                  'Update or remove it with that package (Settings > Apps > Installed apps); this script does not install a second copy or change it.')
        }
    }
    $expected = Get-ExpectedSum $archivePath

    New-Item -ItemType Directory -Force -Path $Root | Out-Null
    $lock = Enter-InstallLock
    $stage = Join-Path $Root ('.stage.' + [Guid]::NewGuid().ToString('N'))
    $target = $null
    $old = $null
    try {
        if ((Get-ChildItem -LiteralPath $Root -Force | Where-Object Name -ne '.lock' | Measure-Object).Count -gt 0 -and -not (Test-Path -LiteralPath (Join-Path $Root $Mark))) {
            Fail "$Root exists but was not created by this installer; refusing to change it"
        }
        if ($Action -eq 'Update' -and -not (Get-Current)) { Fail "Metro is not installed in $Root; use -Action Install" }
        $stagedVersion = Assert-NoInAppUpdate
        New-Item -ItemType Directory -Path $stage | Out-Null
        $privateArchive = Join-Path $stage $name
        Copy-Item -LiteralPath $archivePath -Destination $privateArchive
        $actual = Get-Sha256 $privateArchive
        if ($actual -ne $expected) { Fail "checksum mismatch for $name (expected $expected, got $actual). Download it again; do not install it." }
        Say "Checksum OK: $name"
        $result = Expand-Verified $privateArchive $stage $top
        $m = $result.Manifest
        if ($m.kind -ne 'desktop' -or $m.target -ne 'windows-x64' -or $m.version -ne $version) {
            Fail "RELEASE.json does not describe a Metro $version windows-x64 desktop package"
        }
        foreach ($f in 'Metro.exe', 'metro-server.exe', 'ui\Metro.exe') {
            if (-not (Test-Path -LiteralPath (Join-Path $result.Dir $f) -PathType Leaf)) { Fail "package is missing $f" }
        }
        if ($Action -eq 'Verify') { Say "Archive verified: $name"; return }
        if (Test-Running) { Fail 'Metro is running from this install; close it first' }
        $marker = Join-Path $Root $Mark
        $markEntry = Get-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue
        if ($markEntry -and ($markEntry.PSIsContainer -or $markEntry.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint))) { Fail 'invalid install marker' }
        if (-not $markEntry) {
            Set-Content -LiteralPath $marker -Value "Metro desktop per-user install (managed by $Prog). Your data is in $UserData." -Encoding UTF8
        }

        $previous = Get-Current
        $enable = $true
        if ($NoLocalRunner) { $enable = $false }
        elseif ($LocalRunner) { $enable = $true }
        elseif ($previous) { $enable = Test-Path -LiteralPath (Join-Path $Root "versions\$previous\enable-local-runner") -PathType Leaf }
        if ($enable) { [System.IO.File]::WriteAllBytes((Join-Path $result.Dir 'enable-local-runner'), [byte[]]@()) }

        $versions = Join-Path $Root 'versions'
        New-Item -ItemType Directory -Force -Path $versions | Out-Null
        if ((Get-Item -LiteralPath $versions -Force).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { Fail 'invalid versions directory' }
        $target = Join-Path $versions $version
        if (Test-Path -LiteralPath $target) {
            $old = Join-Path $Root ('.old.' + [Guid]::NewGuid().ToString('N'))
            Move-Item -LiteralPath $target -Destination $old
        }
        Move-Item -LiteralPath $result.Dir -Destination $target
        $current = Join-Path $Root 'current.txt'
        $tmp = Join-Path $stage 'current.txt.new'
        $ptr = [IO.File]::Open($tmp, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try {
            $bytes = [Text.Encoding]::ASCII.GetBytes($version)
            $ptr.Write($bytes, 0, $bytes.Length)
            $ptr.Flush($true)
        } finally { $ptr.Dispose() }
        # File.Replace needs a null backup path; PowerShell binds $null to an empty string here.
        if (Test-Path -LiteralPath $current) { [IO.File]::Replace($tmp, $current, [NullString]::Value) }
        else { [IO.File]::Move($tmp, $current) }
        if ($stagedVersion -and $stagedVersion -ne $version) {
            $superseded = Join-Path $versions $stagedVersion
            if (Test-Path -LiteralPath $superseded) { Remove-Item -LiteralPath $superseded -Recurse -Force }
        }
        if ($old) { Remove-Item -LiteralPath $old -Recurse -Force }
        # A staged or finished in-app update is superseded (its files are gone).
        Remove-Item -LiteralPath (Join-Path $Root '.update.json') -Force -ErrorAction SilentlyContinue

        # Keep a copy of this script for the "Installed apps" uninstall entry.
        $self = Join-Path $Root 'install-metro-windows.ps1'
        if ($PSCommandPath -and ((Resolve-Path -LiteralPath $PSCommandPath).ProviderPath -ne $self)) {
            Copy-Item -LiteralPath $PSCommandPath -Destination $self -Force
        }
        $exe = Join-Path $target 'Metro.exe'
        $existing = if (-not $NoShortcut) { Get-ShortcutTarget (Get-ShortcutPath) }
        if (-not $NoShortcut -and $existing -and -not (Test-OwnShortcut $existing $Root)) {
            Say "Kept the existing Start menu shortcut: it belongs to another Metro installation ($existing)."
        } elseif (-not $NoShortcut) {
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
        if (-not $enable) { Say 'Local runner support is off: Metro won''t set up its built-in runner on its own. Set it up in Metro under Settings > Runners, or install again with -LocalRunner.' }
        elseif ($supported) {
            Say 'Local runner support is on. Metro sets up your personal on-device runner after you sign in; it is not attached to any Space until you choose it under Manage Space.'
        } else {
            Say 'Local runner support was requested, but this Metro build predates it, so no runner will be set up. It takes effect once you update to a build that supports it.'
        }
        Say "Your Metro data stays in $UserData."
    } finally {
        if ($old -and $target -and (Test-Path -LiteralPath $old) -and -not (Test-Path -LiteralPath $target)) {
            Move-Item -LiteralPath $old -Destination $target -ErrorAction SilentlyContinue
        }
        if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue }
        try { Exit-LegacyLock } finally { $lock.Dispose() }
        if ($Action -eq 'Verify') {
            Remove-Item -LiteralPath $Root -Recurse -Force -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath (Get-InstallLockPath) -Force -ErrorAction SilentlyContinue
        }
    }
}

function Uninstall-Metro {
    if (-not $OnWindows) { Fail 'this installer is for Windows' }
    if (-not (Test-Path -LiteralPath $Root)) { Say "Metro is not installed in $Root."; return }
    $lock = Enter-InstallLock
    try {
        if (-not (Test-Path -LiteralPath (Join-Path $Root $Mark))) { Fail "$Root was not created by this installer; refusing to remove it" }
        if (Test-Running) { Fail 'Metro is running; close it first' }
        $null = Assert-NoInAppUpdate
        $lnk = Get-ShortcutPath
        # Only this script's own shortcut; another installer's (such as the MSI's) stays.
        if (Test-OwnShortcut (Get-ShortcutTarget $lnk) $Root) { Remove-Item -LiteralPath $lnk -Force }
        if (Test-Path -LiteralPath $UninstallKey) { Remove-Item -LiteralPath $UninstallKey -Recurse -Force }
        $retired = Join-Path (Split-Path -Parent $Root) ('.metro-uninstall.' + [Guid]::NewGuid().ToString('N'))
        [IO.Directory]::Move($Root, $retired)
        Remove-Item -LiteralPath $retired -Recurse -Force
    } finally { try { Exit-LegacyLock } finally { $lock.Dispose() } }
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
            if ($OnWindows) {
                $msi = @(Get-MsiManagedMetro)
                if ($msi.Count -gt 0) { Say "Also installed by the Metro Windows Installer package (MSI) $($msi -join ' '); manage it in Settings > Apps." }
            }
            Say "Data: $UserData"
        }
    }
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
