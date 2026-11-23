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

"fails=$script:fails"
exit $script:fails
