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
