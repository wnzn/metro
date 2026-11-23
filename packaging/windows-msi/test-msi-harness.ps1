# Checks for test-msi-windows.ps1 and MsiHarness.ps1 that need no Windows:
# parameter/variable name clashes (PowerShell names are case-insensitive),
# collection helpers with 0, 1 and many items, Windows Installer lookups and
# package identity through .NET stand-ins, and the sanitized failure location.
# Set METRO_MSI_FACTS_JSON to Property/Shortcut rows exported from packages that
# build_msi.py built (packaging/test_windows_msi.py does when it has the tools);
# without it, identity checks use rows written here to the same layout.
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'MsiAutomation.ps1')
. (Join-Path $PSScriptRoot 'MsiHarness.ps1')
$Harness = Join-Path $PSScriptRoot 'test-msi-windows.ps1'
$Pwsh = [Environment]::ProcessPath
$script:fails = 0
function Check([bool] $c, [string] $w) { if ($c) { "PASS  $w" } else { $script:fails++; "FAIL  $w" } }

Add-Type -TypeDefinition @'
using System; using System.Collections; using System.Collections.Generic; using System.Runtime.CompilerServices;
public class HStringList : IEnumerable {
  readonly List<string> items; public HStringList(string[] a) { items = new List<string>(a); }
  public int Count { get { return items.Count; } }
  [IndexerName("Item")] public string this[int i] { get { return items[i]; } }
  public IEnumerator GetEnumerator() { return items.GetEnumerator(); } }
public class HRelated { public string[] Next = new string[0];
  [IndexerName("RelatedProducts")] public HStringList this[string uc] { get { return new HStringList(Next); } } }
public class HProductInfo { public Dictionary<string, string> Values = new Dictionary<string, string>();
  [IndexerName("ProductInfo")] public string this[string pc, string attr] { get { return Values[pc + "|" + attr]; } } }
public class HRecord { readonly string[] f; public HRecord(string[] f) { this.f = f; }
  [IndexerName("StringData")] public string this[int i] { get { return f[i - 1]; } } }
public class HView { readonly Queue<HRecord> q = new Queue<HRecord>();
  public HView(List<string[]> rows) { foreach (var r in rows) q.Enqueue(new HRecord(r)); }
  public void Execute() {} public object Fetch() { return q.Count > 0 ? q.Dequeue() : null; } public void Close() {} }
public class HDb { public Dictionary<string, List<string[]>> Tables = new Dictionary<string, List<string[]>>();
  public object OpenView(string sql) { foreach (var t in Tables) if (sql.Contains("FROM `" + t.Key + "`")) return new HView(t.Value);
    throw new ArgumentException("no such table in the stand-in"); } }
public class HOpener { public Dictionary<string, HDb> Dbs = new Dictionary<string, HDb>();
  public object OpenDatabase(string path, int mode) { if (mode != 0) throw new ArgumentException("read-only expected"); return Dbs[path]; } }
'@

# ---- 1. Parameters vs script variables -------------------------------------
# The real failure: `$msi = @{}` assigned the [string] -Msi parameter (same
# variable), so the map became the string 'System.Collections.Hashtable'.
$tmp = Join-Path ([IO.Path]::GetTempPath()) ('msi harness test ' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $old = Join-Path $tmp 'old shape.ps1'
    Set-Content -LiteralPath $old -Value @(
        'param([Parameter(Mandatory = $true)] [string] $FixturesDir, [string] $Msi, [string] $Version)',
        '$ErrorActionPreference = ''Stop''',
        '$msi = @{}; foreach ($n in ''1.0.0'', ''1.1.0'') { $msi[$n] = Join-Path $FixturesDir "f-$n.msi" }',
        'Write-Output "real=$Msi"')
    # Paths with spaces that exist on any platform (Join-Path refuses an absent C: drive off Windows).
    $fxDir = Join-Path $tmp 'fixtures dir'
    $realMsi = Join-Path (Join-Path $tmp 'dist dir') 'Metro-1.0.0-windows-x64.msi'
    $out = (& $Pwsh -NoProfile -NonInteractive -File $old -FixturesDir $fxDir -Msi $realMsi -Version 1.0.0 2>&1) -join "`n"
    Check ($LASTEXITCODE -ne 0 -and $out -match 'Unable to index into an object of type "?System\.String"?') 'old shape reproduces "Unable to index into an object of type System.String" (pwsh -File, [string] -Msi)'
    $new = Join-Path $tmp 'new shape.ps1'
    Set-Content -LiteralPath $new -Value @(
        'param([Parameter(Mandatory = $true)] [string] $FixturesDir, [string] $Msi, [string] $Version)',
        '$ErrorActionPreference = ''Stop''',
        '$fixtureMsi = @{}; foreach ($n in ''1.0.0'', ''1.1.0'') { $fixtureMsi[$n] = Join-Path $FixturesDir "f-$n.msi" }',
        'Write-Output "real=$Msi count=$($fixtureMsi.Count)"')
    $out = (& $Pwsh -NoProfile -NonInteractive -File $new -FixturesDir $fxDir -Msi $realMsi -Version 1.0.0 2>&1) -join "`n"
    Check ($LASTEXITCODE -eq 0 -and $out -ceq "real=$realMsi count=2") 'distinct names keep the real -Msi path and the fixture map'
    $global:LASTEXITCODE = 0
} finally { Remove-Item -LiteralPath $tmp -Recurse -Force }

# Static rule for the harness: outside function bodies, nothing assigns (or
# loops over) a variable whose name equals a parameter's, ignoring case. The one
# allowed case is filling the default of -LogDir itself.
function Get-ParamShadowing([string] $Text) {
    $ast = [Management.Automation.Language.Parser]::ParseInput($Text, [ref] $null, [ref] $null)
    $params = @{}
    if ($ast.ParamBlock) { foreach ($p in $ast.ParamBlock.Parameters) { $params[$p.Name.VariablePath.UserPath.ToLowerInvariant()] = $p.Name.VariablePath.UserPath } }
    $hits = [Collections.Generic.List[string]]::new()
    $nodes = $ast.FindAll({ param($n) $n -is [Management.Automation.Language.AssignmentStatementAst] -or $n -is [Management.Automation.Language.ForEachStatementAst] }, $true)
    foreach ($n in $nodes) {
        $inFunction = $false
        for ($q = $n.Parent; $q; $q = $q.Parent) { if ($q -is [Management.Automation.Language.FunctionDefinitionAst]) { $inFunction = $true; break } }
        if ($inFunction) { continue }
        $target = if ($n -is [Management.Automation.Language.ForEachStatementAst]) { $n.Variable } else { $n.Left }
        while ($target -is [Management.Automation.Language.ConvertExpressionAst]) { $target = $target.Child }
        if ($target -isnot [Management.Automation.Language.VariableExpressionAst]) { continue }
        $name = $target.VariablePath.UserPath
        if ($name -match '^(script|global|local|private):') { $name = $name.Split(':', 2)[1] }
        $key = $name.ToLowerInvariant()
        if ($params.ContainsKey($key) -and -not ($name -ceq 'LogDir' -and $params[$key] -ceq 'LogDir')) {
            $hits.Add("line $($n.Extent.StartLineNumber): `$$name reuses parameter -$($params[$key])")
        }
    }
    return , [string[]] $hits.ToArray()
}
$shadow = Get-ParamShadowing (Get-Content -LiteralPath $Harness -Raw)
Check ($shadow.Count -eq 0) "test-msi-windows.ps1 reuses no parameter name ($($shadow -join '; '))"
$bad = Get-ParamShadowing "param([string] `$Msi)`n`$msi = @{}`nforeach (`$version in 1) { }`nfunction f { `$msi = 1 }"
Check ($bad.Count -eq 1 -and $bad[0] -like 'line 2: $msi reuses parameter -Msi') "the rule catches `$msi = @{} next to [string] `$Msi (got $($bad -join '; '))"

# ---- 2. Get-Tree and Get-SetDifference --------------------------------------
$root = Join-Path ([IO.Path]::GetTempPath()) ('tree with spaces ' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
try {
    # The old Get-Tree returned @(...) through the pipeline: one file became a bare string.
    function Old-Tree([string] $Dir) { @(Get-ChildItem -LiteralPath $Dir -Recurse -Force -File | ForEach-Object { $_.Name } | Sort-Object) }
    $t = Get-Tree $root
    Check ($t -is [string[]] -and $t.Count -eq 0) '0 files: Get-Tree returns an empty [string[]]'
    Check ($null -eq (Old-Tree $root)) '0 files: old Get-Tree returned $null'
    Set-Content -LiteralPath (Join-Path $root 'user-notes.txt') -Value 'mine'
    $t = Get-Tree $root
    Check ($t -is [string[]] -and $t.Count -eq 1 -and $t[0] -ceq 'user-notes.txt') '1 file: Get-Tree returns [string[]] with the whole name at [0]'
    $o = Old-Tree $root
    Check ($o -is [string] -and [string] $o[0] -ceq 'u') "1 file: old Get-Tree returned a bare string, so [0] was 'u'"
    $mine = Join-Path (Join-Path $root 'ui') 'my stuff'
    New-Item -ItemType Directory -Path $mine | Out-Null
    Set-Content -LiteralPath (Join-Path $mine 'x y.txt') -Value 'mine'
    New-Item -ItemType Directory -Path (Join-Path $root 'licenses') | Out-Null
    Set-Content -LiteralPath (Join-Path (Join-Path $root 'licenses') 'THIRD-PARTY NOTICES.txt') -Value 'n'
    $t = Get-Tree $root
    $want = 'licenses/THIRD-PARTY NOTICES.txt|ui/my stuff/x y.txt|user-notes.txt'
    Check ($t -is [string[]] -and $t.Count -eq 3 -and ($t -join '|') -ceq $want) "many files with spaces: ordinal-sorted relative '/' paths ($($t -join ', '))"
} finally { Remove-Item -LiteralPath $root -Recurse -Force }
Check ((Get-SetDifference @('a', 'b') @('b', 'a')) -eq '') 'set difference: same names'
Check ((Get-SetDifference @() ([string[]]::new(0))) -eq '') 'set difference: both empty'
Check ((Get-SetDifference @('user-notes.txt') ([string[]]::new(0))) -eq 'missing [user-notes.txt]; unexpected []') 'set difference: nothing left where one file was expected'
Check ((Get-SetDifference @('a') @('a', 'Metro.exe')) -eq 'missing []; unexpected [Metro.exe]') 'set difference: leftover installed file'
Check ((Get-SetDifference @('A.txt') @('a.txt')) -ne '') 'set difference: names compare exactly'

# ---- 3. Windows Installer lookups ---------------------------------------------
$rel = [HRelated]::new()
foreach ($case in @(@(), @('{A}'), @('{A}', '{B}', '{C}'))) {
    $rel.Next = [string[]] $case
    $r = Get-RelatedProducts $rel '{UC}'
    Check ($r -is [string[]] -and $r.Count -eq $case.Count -and ($r -join ',') -eq ($case -join ',')) "$($case.Count) related product(s): [string[]] of $($case.Count)"
}
$pi = [HProductInfo]::new()
$pi.Values['{P}|InstallLocation'] = 'C:\Users\u\AppData\Local\Metro MSI Test 1\Custom Folder With Spaces\Metro\'
$pi.Values['{P}|AssignmentType'] = '0'
$pi.Values['{P}|VersionString'] = '1.0.99'
$f = Get-ProductFacts $pi '{P}'
Check ($f.AssignmentType -eq '0' -and $f.InstallLocation.TrimEnd('\') -like '*\Custom Folder With Spaces\Metro' -and $f.VersionString -eq '1.0.99') 'product facts: per-user context and install location'

# ---- 4. Package identity (preview and release) ---------------------------------
if ($env:METRO_MSI_FACTS_JSON) {
    $facts = Get-Content -LiteralPath $env:METRO_MSI_FACTS_JSON -Raw | ConvertFrom-Json
    "identity rows: exported from packages built by build_msi.py"
} else {
    $facts = [pscustomobject] @{
        version = '1.0.0'
        release = [pscustomobject] @{ file = 'Metro-1.0.0-windows-x64.msi'
            Property = @(@('ProductName', 'Metro'), @('UpgradeCode', '{29BBE899-7D6A-49A7-B60E-F9644544D683}'), @('ProductCode', '{3D6B2A2E-6F0B-5E52-9C1B-2F6C8E3A1B11}'), @('MetroSemver', '1.0.0'), @('MetroPreview', '0'))
            Shortcut = @(, @('Metro.lnk', 'ProgramMenuFolder')) }
        preview = [pscustomobject] @{ file = 'Metro-1.0.0-windows-x64.unsigned-preview.msi'
            Property = @(@('ProductName', 'Metro (unsigned preview)'), @('UpgradeCode', '{29BBE899-7D6A-49A7-B60E-F9644544D683}'), @('ProductCode', '{3D6B2A2E-6F0B-5E52-9C1B-2F6C8E3A1B11}'), @('MetroSemver', '1.0.0'), @('MetroPreview', '1'))
            Shortcut = @(, @('Metro.lnk', 'ProgramMenuFolder')) }
    }
    "identity rows: written in this test (set METRO_MSI_FACTS_JSON for rows from build_msi.py)"
}
$opener = [HOpener]::new()
$pkg = @{}
foreach ($kind in 'release', 'preview') {
    $spec = $facts.$kind
    $db = [HDb]::new()
    $db.Tables['Property'] = [Collections.Generic.List[string[]]]::new()
    foreach ($r in $spec.Property) { $db.Tables['Property'].Add([string[]] @($r)) }
    $db.Tables['Shortcut'] = [Collections.Generic.List[string[]]]::new()
    foreach ($r in $spec.Shortcut) { $db.Tables['Shortcut'].Add([string[]] @($r)) }
    $path = "C:\dist dir\$($spec.file)"
    $opener.Dbs[$path] = $db
    $pkg[$kind] = Get-MsiPackageFacts $opener $path
}
$v = [string] $facts.version
foreach ($kind in 'release', 'preview') {
    $p = Get-PackageIdentityProblems $pkg[$kind] ([string] $facts.$kind.file) $v
    Check ($p -is [string[]] -and $p.Count -eq 0) "$kind package matches its own file name ($($p -join '; '))"
    Check ($pkg[$kind].Shortcuts -is [string[]] -and $pkg[$kind].Shortcuts.Count -eq 1 -and $pkg[$kind].Shortcuts[0] -ceq 'Metro.lnk') "$kind package: one Start menu shortcut Metro.lnk"
}
$p = Get-PackageIdentityProblems $pkg['preview'] ([string] $facts.release.file) $v
Check ($p.Count -eq 2 -and ($p -join ';') -match 'MetroPreview' -and ($p -join ';') -match 'ProductName') 'preview package under the release file name is refused'
$p = Get-PackageIdentityProblems $pkg['release'] ([string] $facts.preview.file) $v
Check ($p.Count -eq 2) 'release package under the preview file name is refused'
$p = Get-PackageIdentityProblems $pkg['release'] "Metro-9.9.9-windows-x64.msi" '9.9.9'
Check ($p.Count -eq 1 -and $p[0] -like 'MetroSemver*') 'wrong version is refused'
$p = Get-PackageIdentityProblems $pkg['release'] 'metro.msi' $v
Check ($p.Count -eq 1 -and $p[0] -like 'file name*') 'unexpected file name is refused'

# ---- 5. Failure location ------------------------------------------------------
$fdir = Join-Path ([IO.Path]::GetTempPath()) ('private dir ' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fdir | Out-Null
try {
    $fs = Join-Path $fdir 'fail.ps1'
    Set-Content -LiteralPath $fs -Value @('function Inner([string] $Secret) {', '    throw "FAIL  something"', '}', 'Inner ''ARG-VALUE-123''')
    $loc = $null
    try { & $fs } catch { $loc = Format-FailureLocation $_ }
    Check ($loc -like 'at fail.ps1:2; stack: *' -and $loc -match 'at Inner, fail\.ps1: line 2' -and $loc -notmatch 'private dir' -and $loc -notmatch 'ARG-VALUE-123') "failure location names file and line only ($loc)"
} finally { Remove-Item -LiteralPath $fdir -Recurse -Force }

# ---- 6. Windows Installer policy (DisableMsi) -------------------------------------
$st = { param($exists, $kind, $data) @{ KeyExists = $true; ValueExists = $exists; Kind = $kind; Data = $data } }
$cases = @(
    @(@{ KeyExists = $false; ValueExists = $false; Kind = ''; Data = $null }, $true, 'refused', 'absent key on Windows Server'),
    @(@{ KeyExists = $false; ValueExists = $false; Kind = ''; Data = $null }, $false, 'allowed', 'absent key on Windows client'),
    @((& $st $true 'DWord' 0), $true, 'allowed', 'DisableMsi=0 on Server'),
    @((& $st $true 'DWord' 1), $false, 'refused', 'DisableMsi=1 on client'),
    @((& $st $true 'DWord' 2), $false, 'refused', 'DisableMsi=2'),
    @((& $st $true 'DWord' 5), $true, 'refused', 'other number on Server'),
    @((& $st $true 'DWord' 5), $false, 'allowed', 'other number on client'),
    @((& $st $true 'String' '0'), $true, 'refused', 'REG_SZ "0" is not the DWORD policy (Server default)'))
foreach ($c in $cases) {
    $got = Get-BasicUserPolicyExpectation $c[0] $c[1]
    Check ($got -eq $c[2]) "policy expectation: $($c[3]) -> $($c[2]) (got $got)"
}
$hosted = @{ GITHUB_ACTIONS = 'true'; CI = 'true'; RUNNER_ENVIRONMENT = 'github-hosted' }
Check ((Get-PolicyFixtureRefusal $hosted $true) -eq '') 'policy fixture allowed on an elevated GitHub-hosted runner'
Check ((Get-PolicyFixtureRefusal @{ GITHUB_ACTIONS = 'true'; CI = 'true'; RUNNER_ENVIRONMENT = 'self-hosted' } $true) -like 'runner is not GitHub-hosted*') 'policy fixture refused on a self-hosted runner'
Check ((Get-PolicyFixtureRefusal @{ CI = 'true'; RUNNER_ENVIRONMENT = 'github-hosted' } $true) -eq 'not running in GitHub Actions') 'policy fixture refused outside GitHub Actions'
Check ((Get-PolicyFixtureRefusal @{} $true) -ne '') 'policy fixture refused on a developer machine'
Check ((Get-PolicyFixtureRefusal $hosted $false) -like 'the test process is not elevated*') 'policy fixture refused without elevation (none is attempted)'

# Fake registry store: path -> @{ name -> @{ Kind; Data } }.
function New-FakeStore([hashtable] $Keys) {
    $k = $Keys
    return @{
        Keys = $k
        KeyExists = { param($p) $k.ContainsKey($p) }.GetNewClosure()
        Read = { param($p, $n) if ($k.ContainsKey($p) -and $k[$p].ContainsKey($n)) { @{ Exists = $true; Kind = $k[$p][$n].Kind; Data = $k[$p][$n].Data } } else { @{ Exists = $false; Kind = ''; Data = $null } } }.GetNewClosure()
        Write = { param($p, $n, $kind, $data) $k[$p][$n] = @{ Kind = [string] $kind; Data = $data } }.GetNewClosure()
        DeleteValue = { param($p, $n) $k[$p].Remove($n) }.GetNewClosure()
        CreateKey = { param($p) $k[$p] = @{} }.GetNewClosure()
        KeyIsEmpty = { param($p) $k[$p].Count -eq 0 }.GetNewClosure()
        DeleteKey = { param($p) $k.Remove($p) }.GetNewClosure()
    }
}
$key = 'SOFTWARE\Policies\Microsoft\Windows\Installer'
$during = $null
$fs = New-FakeStore @{}
Invoke-WithClientMsiPolicy $fs { $script:during = (& $fs.Read $key 'DisableMsi') }
Check ($during.Exists -and $during.Kind -eq 'DWord' -and [int] $during.Data -eq 0 -and -not $fs.Keys.ContainsKey($key)) 'absent key: DisableMsi=0 during the body, key absent again after'
$fs = New-FakeStore @{ $key = @{ Other = @{ Kind = 'String'; Data = 'x' } } }
Invoke-WithClientMsiPolicy $fs { }
Check ($fs.Keys[$key].Count -eq 1 -and $fs.Keys[$key]['Other'].Data -eq 'x') 'key present, value absent: value removed again, other values untouched'
foreach ($old in @(@{ Kind = 'DWord'; Data = 1 }, @{ Kind = 'String'; Data = '1' })) {
    $fs = New-FakeStore @{ $key = @{ DisableMsi = @{ Kind = $old.Kind; Data = $old.Data } } }
    Invoke-WithClientMsiPolicy $fs { }
    $v = $fs.Keys[$key]['DisableMsi']
    Check ($v.Kind -ceq $old.Kind -and [string] $v.Data -ceq [string] $old.Data) "existing $($old.Kind) $($old.Data): restored with the same kind and data"
}
$fs = New-FakeStore @{ $key = @{ DisableMsi = @{ Kind = 'DWord'; Data = 1 } } }
$msg = ''
try { Invoke-WithClientMsiPolicy $fs { throw 'FAIL  inner install' } } catch { $msg = $_.Exception.Message }
Check ($msg -eq 'FAIL  inner install' -and [int] $fs.Keys[$key]['DisableMsi'].Data -eq 1) 'a failing body is rethrown after the policy is restored'
$fs = New-FakeStore @{}
$msg = ''
try { Invoke-WithClientMsiPolicy $fs { & $fs.Write $key 'SomethingElse' 'DWord' 3 } } catch { $msg = $_.Exception.Message }
Check ($msg -like '*gained other values*' -and $fs.Keys.ContainsKey($key) -and -not $fs.Keys[$key].ContainsKey('DisableMsi')) 'a key that gained other values is not deleted, and the test fails'

# whoami /groups /fo csv /nh lines (name, type, SID, attributes).
$admDeny = '"BUILTIN\Administrators","Alias","S-1-5-32-544","Group used for deny only"'
$admOn = '"BUILTIN\Administrators","Alias","S-1-5-32-544","Mandatory group, Enabled by default, Enabled group, Group owner"'
$users = '"BUILTIN\Users","Alias","S-1-5-32-545","Mandatory group, Enabled by default, Enabled group"'
$medium = '"Mandatory Label\Medium Mandatory Level","Label","S-1-16-8192",""'
$high = '"Mandatory Label\High Mandatory Level","Label","S-1-16-12288",""'
Check ((Get-NonAdminTokenProblem @($users, $admDeny, $medium)) -eq '') 'token: Medium integrity with Administrators deny-only is non-admin'
Check ((Get-NonAdminTokenProblem @($users, $admOn, $high)) -eq 'token has High or System integrity') 'token: an elevated token is rejected'
Check ((Get-NonAdminTokenProblem @($users, $admOn, $medium)) -eq 'Administrators group is enabled in the token') 'token: enabled Administrators group is rejected'
Check ((Get-NonAdminTokenProblem @('')) -eq 'no groups were recorded for the token') 'token: no recorded groups is rejected'

# ---- 7. Installed tree against a manifest ------------------------------------------
$t7 = Join-Path ([IO.Path]::GetTempPath()) ('installed tree ' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path (Join-Path $t7 'ui') -Force | Out-Null
try {
    [IO.File]::WriteAllText((Join-Path $t7 'Metro.exe'), 'a')
    [IO.File]::WriteAllText((Join-Path $t7 'ui/Metro.exe'), 'b')
    [IO.File]::WriteAllText((Join-Path $t7 'RELEASE.json'), '{}')
    [IO.File]::WriteAllBytes((Join-Path $t7 'enable-local-runner'), [byte[]] @())
    $sha = { param($x) ([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($x)) | ForEach-Object { $_.ToString('x2') }) -join '' }
    $files = ('{"Metro.exe": "' + (& $sha 'a') + '", "ui/Metro.exe": "' + (& $sha 'b') + '"}') | ConvertFrom-Json
    Check ((Get-InstalledTreeProblems $t7 $files $true @()).Count -eq 0) 'installed tree: exact files, bytes and empty marker'
    Check ((Get-InstalledTreeProblems $t7 $files $false @())[0] -ceq 'installed files differ: missing []; unexpected [enable-local-runner]') 'installed tree: marker present when off is reported'
    $one = ('{"Metro.exe": "' + (& $sha 'a') + '"}') | ConvertFrom-Json
    Check ((Get-InstalledTreeProblems $t7 $one $true @('ui/Metro.exe')).Count -eq 0) 'installed tree: one-file manifest plus an expected extra file'
    [IO.File]::WriteAllText((Join-Path $t7 'ui/Metro.exe'), 'changed')
    $pr7 = Get-InstalledTreeProblems $t7 $files $true @()
    Check ($pr7 -contains 'ui/Metro.exe bytes differ') "installed tree: changed bytes are reported ($($pr7 -join '; '))"
    [IO.File]::WriteAllText((Join-Path $t7 'enable-local-runner'), 'x')
    $pr7 = Get-InstalledTreeProblems $t7 $files $true @()
    Check ($pr7 -contains 'marker is not empty') "installed tree: non-empty marker is reported ($($pr7 -join '; '))"
    Check ((Get-InstalledTreeProblems (Join-Path $t7 'absent') $files $true @())[0] -like 'folder * does not exist') 'installed tree: missing folder is reported'
} finally { Remove-Item -LiteralPath $t7 -Recurse -Force }
if ($env:METRO_MSI_EXTRACTED_TREE) {
    # A tree msiextract unpacked from a real build_msi.py package, checked against the RELEASE.json inside it.
    $m = Get-Content -LiteralPath (Join-Path $env:METRO_MSI_EXTRACTED_TREE 'RELEASE.json') -Raw | ConvertFrom-Json
    $pr = Get-InstalledTreeProblems $env:METRO_MSI_EXTRACTED_TREE $m.files $true @()
    Check ($pr.Count -eq 0) "built package: its RELEASE.json files match the extracted payload ($($pr -join '; '))"
    "installed tree rows: extracted from a package built by build_msi.py"
}

# ---- 8. Standard-user fixture pieces ---------------------------------------------
$pw1 = New-EphemeralPassword
$pw2 = New-EphemeralPassword
$plain1 = [Net.NetworkCredential]::new('', $pw1).Password   # test only; never printed
$plain2 = [Net.NetworkCredential]::new('', $pw2).Password
Check ($pw1 -is [Security.SecureString] -and $pw1.IsReadOnly() -and $pw1.Length -eq 32) 'ephemeral password is a read-only 32-character SecureString'
Check ($plain1 -cmatch '[A-Z]' -and $plain1 -cmatch '[a-z]' -and $plain1 -match '[0-9]' -and $plain1 -match '[^A-Za-z0-9]' -and $plain1 -cne $plain2) 'ephemeral password meets complexity and differs per call'
$plain1 = $null; $plain2 = $null

$sid = 'S-1-5-21-1-2-3-1001'
function New-Result([string] $Expect, [string] $Sid = 'S-1-5-21-1-2-3-1001', [string[]] $Skip = @(), [string] $Fail = '', [string] $Err = '') {
    $checks = @($script:StandardUserChecks[$Expect] | Where-Object { $Skip -notcontains $_ } | ForEach-Object { @{ name = $_; ok = ($_ -ne $Fail); detail = '' } })
    return (@{ phase = 'host'; expect = $Expect; user_sid = $Sid; checks = $checks; error = $Err } | ConvertTo-Json -Depth 5 | ConvertFrom-Json)
}
Check ((Get-StandardUserResultProblems (New-Result 'refused') $sid 'refused').Count -eq 0) 'standard-user result: complete refusal phase passes'
Check ((Get-StandardUserResultProblems (New-Result 'allowed') $sid 'allowed').Count -eq 0) 'standard-user result: complete install phase passes'
Check ((Get-StandardUserResultProblems (New-Result 'allowed' -Sid 'S-1-5-21-9') $sid 'allowed')[0] -like "result is from SID*") 'standard-user result: another SID is rejected'
Check ((Get-StandardUserResultProblems (New-Result 'refused') $sid 'allowed').Count -gt 0) 'standard-user result: a refusal never passes as the install'
Check ((Get-StandardUserResultProblems (New-Result 'allowed' -Skip @('folder removed')) $sid 'allowed') -contains "check 'folder removed' was not run") 'standard-user result: a skipped check is rejected'
Check ((Get-StandardUserResultProblems (New-Result 'allowed' -Fail 'token is standard') $sid 'allowed') -contains "check 'token is standard' failed: ") 'standard-user result: a failed token check is rejected'
Check ((Get-StandardUserResultProblems (New-Result 'allowed' -Err 'boom') $sid 'allowed') -contains 'child failed: boom') 'standard-user result: a child error is rejected'
Check ((Get-StandardUserResultProblems $null $sid 'allowed')[0] -eq 'no result was written') 'standard-user result: no result is rejected'
$standardUser = @('"BUILTIN\Users","Alias","S-1-5-32-545","Mandatory group, Enabled by default, Enabled group"', '"Mandatory Label\Medium Mandatory Level","Label","S-1-16-8192",""')
Check ((Get-NonAdminTokenProblem $standardUser) -eq '') 'token: a standard user without the Administrators group passes'

# The child records exactly the checks the parent requires.
$child = Join-Path $PSScriptRoot 'standard-user-msi.ps1'
$childAst = [Management.Automation.Language.Parser]::ParseFile($child, [ref] $null, [ref] $null)
$recorded = @($childAst.FindAll({ param($n) $n -is [Management.Automation.Language.CommandAst] -and $n.GetCommandName() -eq 'Record' }, $true) |
    ForEach-Object { $_.CommandElements[1].Value })
$required = @($script:StandardUserChecks['refused'] + $script:StandardUserChecks['allowed'] | Sort-Object -Unique)
Check ((Get-SetDifference $required $recorded) -eq '') "standard-user-msi.ps1 records exactly the required checks ($(Get-SetDifference $required $recorded))"
$childText = Get-Content -LiteralPath $child -Raw
Check ($childText -notmatch '(?i)-Credential|-Verb\s+RunAs|runas|trustlevel|AlwaysInstallElevated|Policies') 'standard-user-msi.ps1 never elevates, switches users or touches policy'

"fails=$script:fails"
exit $script:fails
