# Helpers for test-msi-windows.ps1 (dot-source after MsiAutomation.ps1). They
# take the Windows Installer object as a parameter, so test-msi-harness.ps1 can
# check them with .NET stand-ins on any PowerShell 7.
#
# Every helper that returns a collection returns it with a unary comma: a plain
# return enumerates it, so no item would become $null and one item a bare string
# (and `$x[0]` then a character).

# Files under $Dir as sorted 'a/b.txt' paths (always a [string[]], also for 0 or 1 file).
function Get-Tree([string] $Dir) {
    $root = (Resolve-Path -LiteralPath $Dir).ProviderPath
    $names = [Collections.Generic.List[string]]::new()
    foreach ($f in Get-ChildItem -LiteralPath $Dir -Recurse -Force -File) {
        $names.Add(([IO.Path]::GetRelativePath($root, $f.FullName) -replace '\\', '/'))
    }
    $sorted = [string[]] $names.ToArray()
    [Array]::Sort($sorted, [StringComparer]::Ordinal)
    return , $sorted
}

# '' when both sets hold the same names, else a description of the difference.
function Get-SetDifference([string[]] $Want, [string[]] $Got) {
    $w = [Collections.Generic.HashSet[string]]::new([string[]] @($Want | Where-Object { $null -ne $_ }), [StringComparer]::Ordinal)
    $g = [Collections.Generic.HashSet[string]]::new([string[]] @($Got | Where-Object { $null -ne $_ }), [StringComparer]::Ordinal)
    $missing = @($w | Where-Object { -not $g.Contains($_) } | Sort-Object)
    $extra = @($g | Where-Object { -not $w.Contains($_) } | Sort-Object)
    if ($missing.Count -eq 0 -and $extra.Count -eq 0) { return '' }
    return "missing [$($missing -join ', ')]; unexpected [$($extra -join ', ')]"
}

# Product codes registered for $UpgradeCode (always a [string[]]).
function Get-RelatedProducts($Installer, [string] $UpgradeCode) {
    $list = Invoke-MsiAutomation $Installer 'Installer.RelatedProducts' @($UpgradeCode)
    $count = [int] (Invoke-MsiAutomation $list 'StringList.Count')
    $out = [string[]]::new($count)
    for ($i = 0; $i -lt $count; $i++) { $out[$i] = [string] (Invoke-MsiAutomation $list 'StringList.Item' @($i)) }
    return , $out
}

# Windows Installer's own record of an installed product. AssignmentType is
# '0' for a per-user and '1' for a per-machine installation. (Windows Installer
# writes the Add/Remove Programs key of per-user products under HKLM too, so
# the registry location of that key says nothing about the context.)
function Get-ProductFacts($Installer, [string] $ProductCode) {
    return @{
        InstallLocation = [string] (Invoke-MsiAutomation $Installer 'Installer.ProductInfo' @($ProductCode, 'InstallLocation'))
        AssignmentType  = [string] (Invoke-MsiAutomation $Installer 'Installer.ProductInfo' @($ProductCode, 'AssignmentType'))
        VersionString   = [string] (Invoke-MsiAutomation $Installer 'Installer.ProductInfo' @($ProductCode, 'VersionString'))
    }
}

function Get-MsiQueryRows($Database, [string] $Query, [int] $Columns) {
    $view = Invoke-MsiAutomation $Database 'Database.OpenView' @($Query)
    Invoke-MsiAutomation $view 'View.Execute' | Out-Null
    $rows = [Collections.Generic.List[string[]]]::new()
    while ($true) {
        $record = Invoke-MsiAutomation $view 'View.Fetch'
        if ($null -eq $record) { break }
        $row = [string[]]::new($Columns)
        for ($field = 1; $field -le $Columns; $field++) { $row[$field - 1] = [string] (Invoke-MsiAutomation $record 'Record.StringData' @($field)) }
        $rows.Add($row)
    }
    Invoke-MsiAutomation $view 'View.Close' | Out-Null
    if ([Runtime.InteropServices.Marshal]::IsComObject($view)) { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) | Out-Null }
    return , $rows.ToArray()
}

# What a built package says about itself (read-only): its identity properties
# and the long names of its Start menu shortcuts, as build_msi.py wrote them.
function Get-MsiPackageFacts($Installer, [string] $Path) {
    $db = Invoke-MsiAutomation $Installer 'Installer.OpenDatabase' @($Path, 0)
    $props = @{}
    foreach ($r in (Get-MsiQueryRows $db 'SELECT `Property`, `Value` FROM `Property`' 2)) { $props[$r[0]] = $r[1] }
    $shortcuts = [Collections.Generic.List[string]]::new()
    foreach ($r in (Get-MsiQueryRows $db 'SELECT `Name`, `Directory_` FROM `Shortcut`' 2)) {
        if ($r[1] -eq 'ProgramMenuFolder') { $shortcuts.Add(($r[0] -split '\|')[-1]) }
    }
    if ([Runtime.InteropServices.Marshal]::IsComObject($db)) { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($db) | Out-Null }
    $get = { param($k) if ($props.ContainsKey($k)) { [string] $props[$k] } else { '' } }
    return @{
        ProductName = & $get 'ProductName'; UpgradeCode = & $get 'UpgradeCode'; ProductCode = & $get 'ProductCode'
        MetroSemver = & $get 'MetroSemver'; MetroPreview = & $get 'MetroPreview'; Shortcuts = [string[]] $shortcuts.ToArray()
    }
}

# Problems with a real package's identity ([string[]], empty when none): the
# release UpgradeCode, this version, a preview flag and product name that match
# the file name's `.unsigned-preview.msi` marking, and exactly one Start menu
# shortcut, Metro.lnk (all as build_msi.py writes them for the release identity).
function Get-PackageIdentityProblems($Package, [string] $FileName, [string] $Version) {
    $releaseUpgradeCode = '{29BBE899-7D6A-49A7-B60E-F9644544D683}'
    $preview = $FileName -like '*.unsigned-preview.msi'
    $p = [Collections.Generic.List[string]]::new()
    if ($FileName -cne "Metro-$Version-windows-x64.msi" -and $FileName -cne "Metro-$Version-windows-x64.unsigned-preview.msi") {
        $p.Add("file name $FileName is not Metro-$Version-windows-x64[.unsigned-preview].msi")
    }
    if ($Package.UpgradeCode -ne $releaseUpgradeCode) { $p.Add("UpgradeCode $($Package.UpgradeCode) is not the release one") }
    if ($Package.MetroSemver -ne $Version) { $p.Add("MetroSemver $($Package.MetroSemver) is not $Version") }
    $wantFlag = if ($preview) { '1' } else { '0' }
    if ($Package.MetroPreview -ne $wantFlag) { $p.Add("MetroPreview $($Package.MetroPreview) does not match the file name (want $wantFlag)") }
    $wantName = if ($preview) { 'Metro (unsigned preview)' } else { 'Metro' }
    if ($Package.ProductName -ne $wantName) { $p.Add("ProductName '$($Package.ProductName)' is not '$wantName'") }
    if ($Package.ProductCode -notmatch '^\{[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}\}$') { $p.Add('ProductCode is not a GUID') }
    $sc = @($Package.Shortcuts)
    if ($sc.Count -ne 1 -or $sc[0] -ne 'Metro.lnk') { $p.Add("Start menu shortcuts [$($sc -join ', ')] are not exactly Metro.lnk") }
    return , [string[]] $p.ToArray()
}

# Where an error was raised, without command lines or argument values:
# '<file>:<line>' plus the script stack with file names reduced to their leaf.
function Format-FailureLocation([Management.Automation.ErrorRecord] $Record) {
    $inv = $Record.InvocationInfo
    $file = if ($inv -and $inv.ScriptName) { Split-Path -Leaf $inv.ScriptName } else { '<no script>' }
    $line = if ($inv) { $inv.ScriptLineNumber } else { 0 }
    $stack = @(([string] $Record.ScriptStackTrace) -split "`r?`n" | Where-Object { $_ } | ForEach-Object {
        $_ -replace '(?<=, )(?:[^,]*[\\/])?([^\\/,]+\.ps1|[^\\/,]+\.psm1)(?=: line \d+)', '$1'
    })
    return "at ${file}:$line; stack: $($stack -join ' <- ')"
}

# ---------------------------------------------------------------- installer policy
# DisableMsi (HKLM\Software\Policies\Microsoft\Windows\Installer, REG_DWORD), per
# https://learn.microsoft.com/en-us/windows/win32/msi/disablemsi:
#   0: all installs allowed (what Windows client editions do when it is absent)
#   1: non-elevated per-user installs blocked (error 1625)
#   2: Windows Installer disabled
#   absent, or any other value: client editions allow everything; Windows Server
#   blocks non-elevated per-user installs, as for 1.
$script:MsiPolicyKey = 'SOFTWARE\Policies\Microsoft\Windows\Installer'
$script:MsiPolicyValue = 'DisableMsi'

# 'allowed' or 'refused' for a non-elevated per-user install.
function Get-BasicUserPolicyExpectation($State, [bool] $IsServer) {
    $n = $null
    if ($State.ValueExists -and $State.Kind -eq 'DWord') { $n = [int64] $State.Data }
    if ($n -eq 0) { return 'allowed' }
    if ($n -eq 1 -or $n -eq 2) { return 'refused' }
    if ($IsServer) { return 'refused' } else { return 'allowed' }
}

# '' when the client-policy fixture may change this machine, else why not. Only
# disposable GitHub-hosted runners qualify; never a self-hosted, operator or
# developer machine.
function Get-PolicyFixtureRefusal([hashtable] $Environment, [bool] $IsAdministrator) {
    if ($Environment['GITHUB_ACTIONS'] -cne 'true' -or $Environment['CI'] -cne 'true') { return 'not running in GitHub Actions' }
    if ($Environment['RUNNER_ENVIRONMENT'] -cne 'github-hosted') { return "runner is not GitHub-hosted (RUNNER_ENVIRONMENT=$($Environment['RUNNER_ENVIRONMENT']))" }
    if (-not $IsAdministrator) { return 'the test process is not elevated, so it cannot set machine policy (no elevation is attempted)' }
    return ''
}

# A registry store is a hashtable of scriptblocks (Windows: New-HklmStore; tests: a fake):
#   KeyExists(path) ; Read(path, name) -> @{Exists; Kind; Data} ; Write(path, name, kind, data)
#   DeleteValue(path, name) ; CreateKey(path) ; KeyIsEmpty(path) ; DeleteKey(path)
function New-HklmStore {
    $hklm = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    return @{
        KeyExists   = { param($p) $k = $hklm.OpenSubKey($p); if ($k) { $k.Dispose(); $true } else { $false } }.GetNewClosure()
        Read        = { param($p, $n)
            $k = $hklm.OpenSubKey($p)
            if (-not $k) { return @{ Exists = $false; Kind = ''; Data = $null } }
            try {
                if ($k.GetValueNames() -notcontains $n) { return @{ Exists = $false; Kind = ''; Data = $null } }
                return @{ Exists = $true; Kind = [string] $k.GetValueKind($n)
                          Data = $k.GetValue($n, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) }
            } finally { $k.Dispose() } }.GetNewClosure()
        Write       = { param($p, $n, $kind, $data)
            $k = $hklm.OpenSubKey($p, $true); try { $k.SetValue($n, $data, [Microsoft.Win32.RegistryValueKind] $kind) } finally { $k.Dispose() } }.GetNewClosure()
        DeleteValue = { param($p, $n) $k = $hklm.OpenSubKey($p, $true); try { $k.DeleteValue($n) } finally { $k.Dispose() } }.GetNewClosure()
        CreateKey   = { param($p) $hklm.CreateSubKey($p).Dispose() }.GetNewClosure()
        KeyIsEmpty  = { param($p) $k = $hklm.OpenSubKey($p); try { $k.ValueCount -eq 0 -and $k.SubKeyCount -eq 0 } finally { $k.Dispose() } }.GetNewClosure()
        DeleteKey   = { param($p) $hklm.DeleteSubKey($p, $true) }.GetNewClosure()
    }
}

function Get-RegistryValueState($Store, [string] $Path, [string] $Name) {
    $key = [bool] (& $Store.KeyExists $Path)
    $v = if ($key) { & $Store.Read $Path $Name } else { @{ Exists = $false; Kind = ''; Data = $null } }
    return @{ KeyExists = $key; ValueExists = [bool] $v.Exists; Kind = [string] $v.Kind; Data = $v.Data }
}

function Format-RegistryValueState($State) {
    if (-not $State.KeyExists) { return 'key absent' }
    if (-not $State.ValueExists) { return 'key present, value absent' }
    $d = if ($State.Data -is [array]) { $State.Data -join ',' } else { [string] $State.Data }
    return "value $($State.Kind) $d"
}

function Test-SameRegistryValueState($A, $B) {
    if ($A.KeyExists -ne $B.KeyExists -or $A.ValueExists -ne $B.ValueExists) { return $false }
    if (-not $A.ValueExists) { return $true }
    if ($A.Kind -cne $B.Kind) { return $false }
    if ($A.Data -is [array] -or $B.Data -is [array]) { return (@($A.Data) -join "`0") -ceq (@($B.Data) -join "`0") }
    return [string] $A.Data -ceq [string] $B.Data
}

# Runs $Body with DisableMsi=0 (the Windows client behaviour), then puts back
# exactly what was there: the old kind and data, or no value, or no key. A
# failure to restore is an error; it never deletes anything it did not create.
function Invoke-WithClientMsiPolicy($Store, [scriptblock] $Body) {
    $path = $script:MsiPolicyKey; $name = $script:MsiPolicyValue
    $before = Get-RegistryValueState $Store $path $name
    $bodyError = $null
    try {
        if (-not $before.KeyExists) { & $Store.CreateKey $path }
        & $Store.Write $path $name 'DWord' 0
        try { & $Body } catch { $bodyError = $_ }
    } finally {
        if ($before.ValueExists) { & $Store.Write $path $name $before.Kind $before.Data }
        elseif ((& $Store.KeyExists $path) -and (& $Store.Read $path $name).Exists) { & $Store.DeleteValue $path $name }
        if (-not $before.KeyExists -and (& $Store.KeyExists $path)) {
            if (-not (& $Store.KeyIsEmpty $path)) {
                throw "installer policy key gained other values or subkeys during the test; not deleting it (was absent before)"
            }
            & $Store.DeleteKey $path
        }
        $after = Get-RegistryValueState $Store $path $name
        if (-not (Test-SameRegistryValueState $before $after)) {
            throw "installer policy not restored: before '$(Format-RegistryValueState $before)', after '$(Format-RegistryValueState $after)'"
        }
    }
    if ($bodyError) { throw $bodyError }
}

# A genuinely non-admin token: Medium integrity (S-1-16-8192), not High
# (S-1-16-12288), and the Administrators group (S-1-5-32-544) only for deny.
# $GroupsCsv is `whoami /groups /fo csv /nh` output: name, type, SID, attributes.
function Get-NonAdminTokenProblem([string[]] $GroupsCsv) {
    $sids = @{}
    foreach ($line in $GroupsCsv) {
        if (-not $line) { continue }
        $f = @($line | ConvertFrom-Csv -Header Name, Type, Sid, Attributes)
        if ($f.Count -eq 1 -and $f[0].Sid) { $sids[[string] $f[0].Sid] = [string] $f[0].Attributes }
    }
    if ($sids.Count -eq 0) { return 'no groups were recorded for the token' }
    if ($sids.ContainsKey('S-1-16-12288') -or $sids.ContainsKey('S-1-16-16384')) { return 'token has High or System integrity' }
    if (-not $sids.ContainsKey('S-1-16-8192')) { return 'token is not Medium integrity' }
    if ($sids.ContainsKey('S-1-5-32-544') -and $sids['S-1-5-32-544'] -notmatch 'deny') { return 'Administrators group is enabled in the token' }
    return ''
}

# ---------------------------------------------------------------- installed tree
# Problems ([string[]]) with an installed folder: exactly the payload files
# listed in $Files (RELEASE.json's `files` object as ConvertFrom-Json gives it),
# RELEASE.json, the empty marker when $Marker, plus $Extra; every payload file
# byte-identical.
function Get-InstalledTreeProblems([string] $Dir, $Files, [bool] $Marker, [string[]] $Extra) {
    $p = [Collections.Generic.List[string]]::new()
    if (-not (Test-Path -LiteralPath $Dir -PathType Container)) { $p.Add("folder $Dir does not exist"); return , [string[]] $p.ToArray() }
    $names = @($Files.PSObject.Properties | ForEach-Object { [string] $_.Name })
    if ($names.Count -eq 0) { $p.Add('the manifest lists no files') }
    $want = [string[]] (@($names) + @('RELEASE.json') + @($Extra | Where-Object { $_ }) + $(if ($Marker) { @('enable-local-runner') } else { @() }))
    $diff = Get-SetDifference $want (Get-Tree $Dir)
    if ($diff) { $p.Add("installed files differ: $diff") }
    foreach ($f in $Files.PSObject.Properties) {
        $path = Join-Path $Dir ([string] $f.Name)
        if ((Test-Path -LiteralPath $path -PathType Leaf) -and
            (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -cne ([string] $f.Value).ToLowerInvariant()) {
            $p.Add("$($f.Name) bytes differ")
        }
    }
    $m = Join-Path $Dir 'enable-local-runner'
    if ($Marker -and (Test-Path -LiteralPath $m -PathType Leaf) -and (Get-Item -LiteralPath $m).Length -ne 0) { $p.Add('marker is not empty') }
    return , [string[]] $p.ToArray()
}

# ---------------------------------------------------------------- standard user
# A random password for an ephemeral local account, only ever held as a
# SecureString (never written, printed or passed on a command line).
function New-EphemeralPassword([int] $Length = 32) {
    $sets = 'ABCDEFGHJKLMNPQRSTUVWXYZ', 'abcdefghijkmnopqrstuvwxyz', '23456789', '!#%+-.:=?@_~'
    $all = -join $sets
    while ($true) {
        $bytes = [byte[]]::new($Length)
        [Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
        $chars = [char[]]::new($Length)
        for ($i = 0; $i -lt $Length; $i++) { $chars[$i] = $all[$bytes[$i] % $all.Length] }
        $ok = $true
        foreach ($s in $sets) { if (-not ($chars | Where-Object { $s.IndexOf($_) -ge 0 })) { $ok = $false } }
        if ($ok) {
            $secure = [Security.SecureString]::new()
            foreach ($c in $chars) { $secure.AppendChar($c) }
            [Array]::Clear($chars, 0, $chars.Length); [Array]::Clear($bytes, 0, $bytes.Length)
            $secure.MakeReadOnly()
            return $secure
        }
        [Array]::Clear($chars, 0, $chars.Length); [Array]::Clear($bytes, 0, $bytes.Length)
    }
}

# Checks each standard-user phase must report (standard-user-msi.ps1 writes them).
$script:StandardUserChecks = @{
    refused = @('token is standard', 'environment from own profile', 'profile belongs to own SID', 'no product before',
                'msiexec refused by policy (1625)', 'nothing registered', 'nothing installed')
    allowed = @('token is standard', 'environment from own profile', 'profile belongs to own SID', 'no product before', 'msiexec installed (0)', 'one per-user product', 'install location',
                'installed files', 'remembered folder in own HKCU', 'own Start menu shortcut', 'msiexec uninstalled (0)',
                'product removed', 'folder removed', 'shortcut removed')
}

# Problems ([string[]]) with a phase result from standard-user-msi.ps1: it must
# come from the ephemeral account's SID with a standard (Medium, non-admin)
# token, and report every check of its expectation as passed.
# $ProfilePath is the account's profile as Windows records it for $Sid
# (Win32_UserProfile); $ParentProfile is the test runner's own profile.
function Get-StandardUserResultProblems($Result, [string] $Sid, [string] $Expect, [string] $ProfilePath, [string] $ParentProfile) {
    $p = [Collections.Generic.List[string]]::new()
    if ($null -eq $Result) { $p.Add('no result was written'); return , [string[]] $p.ToArray() }
    if ([string] $Result.user_sid -cne $Sid) { $p.Add("result is from SID '$($Result.user_sid)', not the test account") }
    $own = [string] $Result.profile_dir
    if (-not $ProfilePath -or $own -ne $ProfilePath) { $p.Add("child worked in profile '$own', not the account's profile '$ProfilePath'") }
    if ($ParentProfile -and $own -eq $ParentProfile) { $p.Add("child worked in the test runner's own profile '$own'") }
    if ([string] $Result.expect -cne $Expect) { $p.Add("result is for '$($Result.expect)', expected '$Expect'") }
    if ($Result.error) { $p.Add("child failed: $($Result.error)") }
    $checks = @{}
    foreach ($c in @($Result.checks)) { if ($c) { $checks[[string] $c.name] = $c } }
    foreach ($name in $script:StandardUserChecks[$Expect]) {
        if (-not $checks.ContainsKey($name)) { $p.Add("check '$name' was not run") }
        elseif ($checks[$name].ok -ne $true) { $p.Add("check '$name' failed: $($checks[$name].detail)") }
    }
    return , [string[]] $p.ToArray()
}

# ---------------------------------------------------------------- own environment
# Start-Process -Credential gives the child the *parent's* environment block, so
# USERPROFILE, LOCALAPPDATA, TEMP (and every known folder and msiexec resolved
# from them) still point at the parent's profile. The child replaces it with the
# block Windows builds for its own token (CreateEnvironmentBlock, bInherit FALSE)
# and resolves known folders for that token explicitly.

# NAME=VALUE entries of a NUL-separated environment block (the '=C:' style
# per-drive entries are skipped), as a case-insensitive ordered dictionary.
function ConvertFrom-EnvironmentBlock([string] $Block) {
    $d = [Collections.Specialized.OrderedDictionary]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $Block.Split([char] 0)) {
        if (-not $entry -or $entry.StartsWith('=')) { continue }
        $i = $entry.IndexOf('=')
        if ($i -lt 1) { continue }
        $d[$entry.Substring(0, $i)] = $entry.Substring($i + 1)
    }
    return , $d
}

# What to set and remove so the process environment equals $Target (names compare
# case-insensitively), keeping only $Keep from the current one.
function Get-EnvironmentPlan($Current, $Target, [string[]] $Keep) {
    $remove = [Collections.Generic.List[string]]::new()
    foreach ($name in @($Current.Keys)) {
        if (-not $Target.Contains($name) -and -not (@($Keep) -contains $name)) { $remove.Add([string] $name) }
    }
    $set = [ordered] @{}
    foreach ($name in $Target.Keys) { if ($Current[$name] -cne $Target[$name]) { $set[$name] = $Target[$name] } }
    return @{ Set = $set; Remove = [string[]] $remove.ToArray() }
}

# Problems ([string[]]) with the account's resolved profile: it must be the
# profile Windows lists for $Sid (ProfileList ProfileImagePath), and the
# environment and known folders must all be inside it.
function Get-OwnProfileProblems([string] $ProfileDir, [string] $ProfileImagePath, $Folders, $Environment) {
    $p = [Collections.Generic.List[string]]::new()
    if (-not $ProfileDir) { $p.Add('no profile directory'); return , [string[]] $p.ToArray() }
    $prefix = $ProfileDir.TrimEnd('\') + '\'
    $inside = { param($x) $x -and $x.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) }
    if ($ProfileImagePath -ne $ProfileDir) { $p.Add("profile '$ProfileDir' is not the one listed for this SID ('$ProfileImagePath')") }
    if ($Environment['USERPROFILE'] -ne $ProfileDir) { $p.Add("USERPROFILE is '$($Environment['USERPROFILE'])'") }
    foreach ($k in 'LOCALAPPDATA', 'APPDATA', 'TEMP', 'TMP') {
        if (-not (& $inside $Environment[$k])) { $p.Add("$k '$($Environment[$k])' is outside the profile") }
    }
    foreach ($k in @($Folders.Keys)) {
        if ($k -eq 'Profile') { if ($Folders[$k] -ne $ProfileDir) { $p.Add("known folder Profile is '$($Folders[$k])'") } }
        elseif (-not (& $inside $Folders[$k])) { $p.Add("known folder $k '$($Folders[$k])' is outside the profile") }
    }
    if ($Folders['LocalAppData'] -ne $Environment['LOCALAPPDATA']) { $p.Add('LOCALAPPDATA differs from the LocalAppData known folder') }
    return , [string[]] $p.ToArray()
}

$script:OwnUserSource = @'
using System; using System.ComponentModel; using System.Runtime.InteropServices; using System.Text;
public static class MetroOwnUser {
    const uint TOKEN_DUPLICATE = 0x0002, TOKEN_IMPERSONATE = 0x0004, TOKEN_QUERY = 0x0008;
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool CloseHandle(IntPtr h);
    [DllImport("advapi32.dll", SetLastError = true)] static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("userenv.dll", SetLastError = true)] static extern bool CreateEnvironmentBlock(out IntPtr env, IntPtr token, bool inherit);
    [DllImport("userenv.dll", SetLastError = true)] static extern bool DestroyEnvironmentBlock(IntPtr env);
    [DllImport("userenv.dll", SetLastError = true, CharSet = CharSet.Unicode)] static extern bool GetUserProfileDirectoryW(IntPtr token, StringBuilder dir, ref uint size);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)] static extern uint GetLongPathNameW(string shortPath, StringBuilder longPath, uint size);
    [DllImport("shell32.dll")] static extern int SHGetKnownFolderPath([MarshalAs(UnmanagedType.LPStruct)] Guid id, uint flags, IntPtr token, out IntPtr path);
    // This process's own token, for reading only (documented access rights of the calls below).
    static IntPtr OwnToken() {
        IntPtr t;
        if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE, out t)) throw new Win32Exception();
        return t;
    }
    public static string EnvironmentBlock() {
        IntPtr t = OwnToken();
        try {
            IntPtr env;
            if (!CreateEnvironmentBlock(out env, t, false)) throw new Win32Exception();
            try {
                var sb = new StringBuilder(); int off = 0;
                while (true) {
                    string s = Marshal.PtrToStringUni(IntPtr.Add(env, off));
                    if (string.IsNullOrEmpty(s)) break;
                    sb.Append(s).Append('\0'); off += (s.Length + 1) * 2;
                }
                return sb.ToString();
            } finally { DestroyEnvironmentBlock(env); }
        } finally { CloseHandle(t); }
    }
    public static string ProfileDirectory() {
        IntPtr t = OwnToken();
        try {
            uint size = 0; GetUserProfileDirectoryW(t, null, ref size);
            var sb = new StringBuilder((int) size + 1);
            if (!GetUserProfileDirectoryW(t, sb, ref size)) throw new Win32Exception();
            return sb.ToString();
        } finally { CloseHandle(t); }
    }
    // Long form of an existing path (8.3 components such as RUNNER~1 expanded).
    public static string LongPath(string path) {
        var sb = new StringBuilder(1024);
        uint n = GetLongPathNameW(path, sb, (uint) sb.Capacity);
        if (n == 0 || n >= sb.Capacity) throw new Win32Exception();
        return sb.ToString();
    }
    public static string KnownFolder(string id) {
        IntPtr t = OwnToken();
        try {
            IntPtr p; int hr = SHGetKnownFolderPath(new Guid(id), 0, t, out p);
            if (hr != 0) Marshal.ThrowExceptionForHR(hr);
            try { return Marshal.PtrToStringUni(p); } finally { Marshal.FreeCoTaskMem(p); }
        } finally { CloseHandle(t); }
    }
}
'@

# Windows only. Replaces this process's environment with its own token's and
# returns @{ ProfileDir; Folders; Inherited } (Inherited: USERPROFILE before).
function Initialize-OwnUserEnvironment {
    Add-Type -TypeDefinition $script:OwnUserSource
    $inherited = $env:USERPROFILE
    $target = ConvertFrom-EnvironmentBlock ([MetroOwnUser]::EnvironmentBlock())
    $current = [Collections.Specialized.OrderedDictionary]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($e in [Environment]::GetEnvironmentVariables('Process').GetEnumerator()) { $current[[string] $e.Key] = [string] $e.Value }
    $plan = Get-EnvironmentPlan $current $target @('PSModulePath')
    foreach ($name in $plan.Remove) { [Environment]::SetEnvironmentVariable($name, $null, 'Process') }
    foreach ($name in $plan.Set.Keys) { [Environment]::SetEnvironmentVariable($name, $plan.Set[$name], 'Process') }
    $folders = [ordered] @{
        Profile      = [MetroOwnUser]::KnownFolder('5E6C858F-0E22-4760-9AFE-EA3317B67173')
        LocalAppData = [MetroOwnUser]::KnownFolder('F1B32785-6FBA-4FCF-9D55-7B8E7F157091')
        RoamingAppData = [MetroOwnUser]::KnownFolder('3EB685DB-65F9-4CF6-A03A-E3EF65729F3D')
        Programs     = [MetroOwnUser]::KnownFolder('A77F5D77-2E2B-44C3-A6A2-ABA601054A51')
    }
    return @{ ProfileDir = [MetroOwnUser]::ProfileDirectory(); Folders = $folders; Inherited = $inherited }
}
