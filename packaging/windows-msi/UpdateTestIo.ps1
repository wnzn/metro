<#
  Shared I/O for the in-app MSI update tests (test-msi-update.ps1,
  msi-update-scenarios.ps1). The app replaces its files with an atomic
  rename, deletes them and holds them open while the test reads them, so
  every read of an app file goes through Read-SharedBytes:
  - it opens with ReadWrite and Delete sharing, so a test read never blocks
    the app's rename or delete;
  - a missing file (or folder) is $null;
  - a sharing violation or a file pending deletion (access denied) is read
    again until $Seconds pass, then thrown; it is never taken as absent.
#>

function Read-SharedBytes([string] $Path, [int] $Seconds = 10) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ($true) {
        try {
            $fs = [IO.FileStream]::new($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare] 'ReadWrite, Delete')
            try {
                $ms = [IO.MemoryStream]::new()
                $fs.CopyTo($ms)
                return , $ms.ToArray()
            } finally { $fs.Dispose() }
        } catch [IO.FileNotFoundException], [IO.DirectoryNotFoundException] {
            return $null
        } catch [IO.IOException], [UnauthorizedAccessException] {
            if ([DateTime]::UtcNow -ge $deadline) { throw }
            Start-Sleep -Milliseconds 50
        }
    }
}

function Read-SharedText([string] $Path, [int] $Seconds = 10) {
    $b = Read-SharedBytes $Path $Seconds
    if ($null -eq $b) { return $null }
    return [Text.UTF8Encoding]::new($false).GetString($b).TrimStart([char] 0xFEFF)
}

# A JSON file the app writes atomically: whole or absent.
function Read-SharedJson([string] $Path, [int] $Seconds = 10) {
    $t = Read-SharedText $Path $Seconds
    if ($null -eq $t) { return $null }
    return $t | ConvertFrom-Json
}

# A JSON-lines log appended by other processes: only complete lines (a line
# being written has no newline yet). Always an array: assign it before
# piping (`$l = Read-SharedJsonLines ...; $l | ...`).
function Read-SharedJsonLines([string] $Path, [int] $Seconds = 10) {
    $t = Read-SharedText $Path $Seconds
    if (-not $t) { return , @() }
    $end = $t.LastIndexOf("`n")
    if ($end -lt 0) { return , @() }
    $lines = $t.Substring(0, $end).Split("`n") | ForEach-Object { $_.TrimEnd("`r") } | Where-Object { $_ }
    return , @($lines | ForEach-Object { $_ | ConvertFrom-Json })
}

function Get-SharedSha256([string] $Path, [int] $Seconds = 10) {
    $b = Read-SharedBytes $Path $Seconds
    if ($null -eq $b) { return $null }
    $sha = [Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($sha.ComputeHash($b)) -replace '-', '').ToLowerInvariant() }
    finally { $sha.Dispose() }
}

# A property that may be missing (StrictMode throws on a missing one).
function Get-Prop($Object, [string] $Name) {
    if ($null -eq $Object) { return $null }
    $p = $Object.PSObject.Properties[$Name]
    if ($p) { return $p.Value }
    return $null
}
