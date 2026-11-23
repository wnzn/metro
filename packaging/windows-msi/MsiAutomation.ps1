# Typed, late-bound calls into the Windows Installer automation interface
# (msi.dll, ProgID WindowsInstaller.Installer). Dot-source this file.
#
# Why: WindowsInstaller.Installer exposes no members to PowerShell's COM
# adapter, so calls go through Type.InvokeMember (IDispatch). InvokeMember hands
# each argument to the COM marshaler as is. A PowerShell-wrapped value (for
# example `$_` from ForEach-Object bound into an [object[]] parameter) is a
# PSObject, which has no VARIANT form, and IDispatch::Invoke fails with
# DISP_E_TYPEMISMATCH (0x80020005). Here every member is declared with its
# documented signature, and every argument is unwrapped and converted to
# exactly that type before the call.
#
# Signatures (Windows Installer automation reference, msi.dll):
#   Installer.OpenDatabase(DatabasePath As String, OpenMode As Long) As Database   method
#   Installer.ProductState(Product As String) As MsiInstallState (Long)            property get
#   Installer.ProductInfo(Product As String, Attribute As String) As String        property get
#   Installer.RelatedProducts(UpgradeCode As String) As StringList                 property get
#   Database.OpenView(Sql As String) As View                                       method
#   Database.Commit()                                                              method
#   View.Execute([Params As Record]): called here without parameters               method
#   View.Fetch() As Record (Nothing after the last row)                            method
#   View.Close()                                                                   method
#   Record.StringData(Field As Long) As String                                     property get
#   StringList.Count As Long                                                       property get
#   StringList.Item(Index As Long) As String                                       property get

$script:MsiAutomationMembers = @{
    'Installer.OpenDatabase'    = @('Method', [string], [int])
    'Installer.ProductState'    = @('Property', [string])
    'Installer.ProductInfo'     = @('Property', [string], [string])
    'Installer.RelatedProducts' = @('Property', [string])
    'Database.OpenView'         = @('Method', [string])
    'Database.Commit'           = @('Method')
    'View.Execute'              = @('Method')
    'View.Fetch'                = @('Method')
    'View.Close'                = @('Method')
    'Record.StringData'         = @('Property', [int])
    'StringList.Count'          = @('Property')
    'StringList.Item'           = @('Property', [int])
}

# Fills a fresh [object[]] with raw .NET values of exactly the declared types.
# Values are assigned straight from casts (never from function output), so no
# PSObject wrapper can reach the COM marshaler.
function New-MsiAutomationArguments([string] $Member, [object[]] $Arguments) {
    $sig = $script:MsiAutomationMembers[$Member]
    if (-not $sig) { throw "MsiAutomation: $Member is not a declared Windows Installer automation member" }
    $count = $sig.Count - 1
    if ($Arguments.Count -ne $count) { throw "MsiAutomation: $Member takes $count argument(s), got $($Arguments.Count)" }
    $typed = [object[]]::new($count)
    for ($i = 0; $i -lt $count; $i++) {
        $type = $sig[$i + 1]
        if ($null -eq $Arguments[$i]) { throw "MsiAutomation: $Member argument $i is null; expected $($type.FullName)" }
        $base = $Arguments[$i].PSObject.BaseObject
        if ($type -eq [string]) {
            if ($base -isnot [string]) { throw "MsiAutomation: $Member argument $i is $($base.GetType().FullName); expected System.String" }
            $typed[$i] = [string] $base
        } elseif ($type -eq [int]) {
            if (-not ($base -is [int] -or $base -is [long] -or $base -is [short] -or $base -is [byte])) {
                throw "MsiAutomation: $Member argument $i is $($base.GetType().FullName); expected System.Int32"
            }
            $typed[$i] = [int] $base
        } else {
            throw "MsiAutomation: $Member declares unsupported type $($type.FullName)"
        }
    }
    return , $typed
}

function Invoke-MsiAutomation {
    param([Parameter(Mandatory)] [object] $Object, [Parameter(Mandatory)] [string] $Member, [object[]] $Arguments = @())
    $checked = New-MsiAutomationArguments $Member $Arguments
    $typed = [object[]]::new($checked.Count)
    for ($i = 0; $i -lt $checked.Count; $i++) { $typed[$i] = $checked[$i].PSObject.BaseObject }
    $target = $Object.PSObject.BaseObject
    $kind = $script:MsiAutomationMembers[$Member][0]
    $flags = if ($kind -eq 'Method') { [Reflection.BindingFlags]::InvokeMethod } else { [Reflection.BindingFlags]::GetProperty }
    $name = $Member.Split('.')[1]
    try {
        return $target.GetType().InvokeMember($name, $flags, $null, $target, $typed, [Globalization.CultureInfo]::InvariantCulture)
    } catch [Management.Automation.MethodInvocationException] {
        # Re-thrown with the operation and types only (argument values such as paths or SQL are not logged).
        $e = $_.Exception.InnerException
        while ($e -is [Reflection.TargetInvocationException] -and $e.InnerException) { $e = $e.InnerException }
        $types = ($typed | ForEach-Object { $_.GetType().FullName }) -join ', '
        throw ("MsiAutomation: {0} failed ({1}, BindingFlags {2}, target {3}, argument types [{4}]): HRESULT 0x{5:X8} {6}" -f
            $Member, $kind, $flags, $target.GetType().FullName, $types, $e.HResult, $e.Message.Trim())
    }
}
