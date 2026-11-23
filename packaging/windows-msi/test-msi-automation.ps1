# Checks for MsiAutomation.ps1 with .NET stand-ins for the Windows Installer
# automation objects (same member names and argument kinds; StringList is
# enumerable like the COM StringList). Runs on any PowerShell 7 (no COM needed);
# packaging/test_windows_msi.py runs it when pwsh is available.
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'MsiAutomation.ps1')
Add-Type -TypeDefinition @'
using System; using System.Collections; using System.Collections.Generic; using System.Runtime.CompilerServices;
public class FakeStringList : IEnumerable {
  readonly List<string> items; public FakeStringList(string[] a) { items = new List<string>(a); }
  public int Count { get { return items.Count; } }
  [IndexerName("Item")] public string this[int i] { get { return items[i]; } }
  public IEnumerator GetEnumerator() { return items.GetEnumerator(); } }
public class FakeView2 { public void Close() {} public object Fetch() { return null; } }
public class FakeInstaller2 {
  public string[] Next = new string[0]; public FakeStringList Last;
  [IndexerName("RelatedProducts")] public FakeStringList this[string uc] { get { Last = new FakeStringList(Next); return Last; } } }
'@
$fails = 0
function Check([bool] $c, [string] $w) { if ($c) { "PASS  $w" } else { $script:fails++; "FAIL  $w" } }
function Old-Invoke($o, $n, $a) { return $o.GetType().InvokeMember($n, [Reflection.BindingFlags]::GetProperty, $null, $o, $a) }
# Same sequence as Get-Related in test-msi-windows.ps1.
function Get-RelatedLike($installer, [string] $uc) {
    $list = Invoke-MsiAutomation $installer 'Installer.RelatedProducts' @($uc)
    $count = [int] (Invoke-MsiAutomation $list 'StringList.Count')
    $out = [string[]]::new($count)
    for ($i = 0; $i -lt $count; $i++) { $out[$i] = [string] (Invoke-MsiAutomation $list 'StringList.Item' @($i)) }
    return ,$out
}
$inst = [FakeInstaller2]::new()
foreach ($case in @(@(), @('{A}'), @('{A}', '{B}', '{C}'))) {
    $inst.Next = [string[]] $case
    $n = $case.Count
    $list = Invoke-MsiAutomation $inst 'Installer.RelatedProducts' @('{UC}')
    Check ($null -ne $list -and [object]::ReferenceEquals($list.PSObject.BaseObject, $inst.Last)) "$n item(s): RelatedProducts returns the StringList object itself"
    Check ($list.PSObject.BaseObject -is [FakeStringList]) "$n item(s): type preserved"
    $old = Old-Invoke $inst 'RelatedProducts' @('{UC}')
    $oldKind = if ($null -eq $old) { 'null' } else { $old.GetType().Name }
    "      old 'return InvokeMember(...)' gave: $oldKind"
    $r = Get-RelatedLike $inst '{UC}'
    Check ($r -is [string[]] -and $r.Count -eq $n -and (($r -join ',') -eq ($case -join ','))) "$n item(s): Get-Related sequence returns $n product code(s)"
    Check ((Get-RelatedLike $inst '{UC}').Count -eq $n) "$n item(s): .Count on the call result is $n"
}
$v = [FakeView2]::new()
$closed = Invoke-MsiAutomation $v 'View.Close'
Check ($null -eq $closed) 'void method returns $null (one object, not an error)'
Check (@(Invoke-MsiAutomation $v 'View.Close').Count -eq 1 -and $null -eq @(Invoke-MsiAutomation $v 'View.Close')[0]) 'void method emits exactly one $null'
Check ($null -eq (Invoke-MsiAutomation $v 'View.Fetch')) 'Fetch past the last row returns $null'
Invoke-MsiAutomation $v 'View.Close' | Out-Null
Add-Type -TypeDefinition @'
using System; using System.Runtime.CompilerServices;
public static class Probe { public static string Kinds(object[] a) { var k = new string[a.Length]; for (int i = 0; i < a.Length; i++) k[i] = a[i] == null ? "null" : a[i].GetType().FullName; return string.Join(",", k); } }
public class FakeRecord { public int Seen; [IndexerName("StringData")] public string this[int field] { get { if (field < 1) throw new ArgumentException(); return "col" + field; } } }
public class FakeView { int n; public void Execute() {} public object Fetch() { return n++ < 2 ? new FakeRecord() : null; } public void Close() {} }
public class FakeDb { public object OpenView(string sql) { return new FakeView(); } public void Commit() {} }
public class FakeList { public int Count { get { return 2; } } [IndexerName("Item")] public string this[int i] { get { return "{P" + i + "}"; } } }
public class FakeInstaller { public object OpenDatabase(string path, int mode) { return new FakeDb(); }
  [IndexerName("ProductState")] public int this[string pc] { get { return 5; } }
  public object RelatedProducts(string uc) { return new FakeList(); } }
'@
$fx = [FakeInstaller]::new()
$a = @(1..2 | ForEach-Object { [Probe]::Kinds((New-MsiAutomationArguments 'Record.StringData' @($_))) })
Check (($a -join '|') -eq 'System.Int32|System.Int32') 'pipeline $_ arguments reach InvokeMember as System.Int32, not PSObject'
$db = Invoke-MsiAutomation $fx 'Installer.OpenDatabase' @('C:\x y\a.msi', 0)
Check ($db.PSObject.BaseObject -is [FakeDb]) 'OpenDatabase returns the Database object'
$view = Invoke-MsiAutomation $db 'Database.OpenView' @('SELECT 1')
Invoke-MsiAutomation $view 'View.Execute' | Out-Null
$rows = @()
while ($true) {
    $r = Invoke-MsiAutomation $view 'View.Fetch'
    if ($null -eq $r) { break }
    $row = [string[]]::new(2)
    for ($f = 1; $f -le 2; $f++) { $row[$f - 1] = [string] (Invoke-MsiAutomation $r 'Record.StringData' @($f)) }
    $rows += , $row
}
Check ($rows.Count -eq 2 -and ($rows[1] -join '/') -eq 'col1/col2') 'Fetch/StringData rows'
Check ((Invoke-MsiAutomation $fx 'Installer.ProductState' @('{PC}')) -eq 5) 'ProductState returns Int32'
foreach ($bad in @(@('Record.StringData', @('1')), @('Installer.OpenDatabase', @('a')), @('Nope.X', @()), @('Installer.ProductState', @(5)))) {
    $refused = $false
    try { Invoke-MsiAutomation $fx $bad[0] $bad[1] | Out-Null } catch { $refused = $_.Exception.Message -like 'MsiAutomation: *' }
    Check $refused "refuses $($bad[0]) with wrong arguments"
}
$msg = ''
try { Invoke-MsiAutomation ([FakeRecord]::new()) 'Record.StringData' @(0) | Out-Null } catch { $msg = $_.Exception.Message }
Check ($msg -match '^MsiAutomation: Record\.StringData failed \(Property, BindingFlags GetProperty, target FakeRecord, argument types \[System\.Int32\]\): HRESULT 0x80070057') 'failure names member, flags, target, types and HRESULT'
Check ($msg -notmatch 'C:\\x y') 'failure does not log argument values'
"fails=$fails"
exit $fails
