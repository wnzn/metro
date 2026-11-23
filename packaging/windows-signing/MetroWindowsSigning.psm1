# Metro Windows signing and Windows trust checks (PowerShell 7 on Windows).
#
# Signs only files listed by `winsign.py prepare` (or the one MSI), each checked
# against a fixed allowlist below the run's work directory. Never generates,
# imports, exports or prints a key or certificate. Two backends share the same
# SignTool command line (SHA-256 file digest, RFC 3161 SHA-256 timestamp):
#   azure : Azure Artifact Signing (public trust) via its SignTool dlib and
#           GitHub OIDC workload identity; no stored secret.
#   kms   : a certificate whose private key lives in a pre-provisioned HSM or
#           KSP on a restricted self-hosted runner, selected by SHA-1 thumbprint.
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$MinSignToolVersion = [Version] '10.0.22621.755'
$CodeSigningEku = '1.3.6.1.5.5.7.3.3'
$TimeStampingEku = '1.3.6.1.5.5.7.3.8'
$AllowedTargets = @(
    '^sign\\Metro-[0-9A-Za-z.\-]+-windows-x64\\(Metro\.exe|metro-server\.exe|metro-tui\.exe|ui\\Metro\.exe)$',
    '^sign\\scripts\\(install-metro-windows|metro-runner-container)\.ps1$',
    '^msi\\Metro-[0-9A-Za-z.\-]+-windows-x64\.msi$'
)

function Fail([string] $Message) { throw "MetroWindowsSigning: $Message" }

function Get-MetroSignTool {
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $candidates = Get-ChildItem -LiteralPath $kits -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^10\.0\.\d+\.\d+$' } |
        ForEach-Object { Join-Path $_.FullName 'x64\signtool.exe' } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        ForEach-Object { [pscustomobject] @{ Path = $_; Version = [Version] (Get-Item -LiteralPath $_).VersionInfo.FileVersionRaw } } |
        Sort-Object Version -Descending
    $tool = $candidates | Select-Object -First 1
    if (-not $tool -or $tool.Version -lt $MinSignToolVersion) { Fail "Windows SDK SignTool $MinSignToolVersion or newer (x64) is required" }
    $sig = Get-AuthenticodeSignature -LiteralPath $tool.Path
    if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch '(^|, )O=Microsoft Corporation(,|$)') {
        Fail "SignTool at $($tool.Path) is not a valid Microsoft-signed binary"
    }
    return $tool.Path
}

function Resolve-Target([string] $Path, [string] $WorkRoot) {
    $root = (Resolve-Path -LiteralPath $WorkRoot).ProviderPath.TrimEnd('\') + '\'
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { Fail "not a regular file: $Path" }
    $full = $item.FullName
    if (-not $full.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) { Fail "refusing to sign outside the work directory: $full" }
    $rel = $full.Substring($root.Length)
    if (-not ($AllowedTargets | Where-Object { $rel -cmatch $_ })) { Fail "refusing to sign a file that is not on the allowlist: $rel" }
    # No parent of the target may be a link either.
    $parent = $item.Directory
    while ($parent -and $parent.FullName.Length -ge $root.Length) {
        if ($parent.Attributes -band [IO.FileAttributes]::ReparsePoint) { Fail "link in the path to $rel" }
        $parent = $parent.Parent
    }
    return $full
}

function Invoke-MetroSigning {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)] [ValidateSet('azure', 'kms')] [string] $Backend,
        [Parameter(Mandatory)] [string] $WorkRoot,
        [Parameter(Mandatory)] [string[]] $Files,
        [Parameter(Mandatory)] [string] $TimestampUrl,
        [string] $Dlib,
        [string] $MetadataFile,
        [string] $Thumbprint,
        [switch] $MachineStore
    )
    if ($TimestampUrl -notmatch '^https?://[A-Za-z0-9.\-]+(:\d+)?(/[A-Za-z0-9._~/\-]*)?$') { Fail 'invalid RFC 3161 timestamp URL' }
    $signtool = Get-MetroSignTool
    $common = @('sign', '/fd', 'SHA256', '/tr', $TimestampUrl, '/td', 'SHA256', '/d', 'Metro')
    switch ($Backend) {
        'azure' {
            foreach ($f in $Dlib, $MetadataFile) { if (-not $f -or -not (Test-Path -LiteralPath $f -PathType Leaf)) { Fail 'Artifact Signing dlib and metadata are required' } }
            $backendArgs = @('/dlib', $Dlib, '/dmdf', $MetadataFile)
        }
        'kms' {
            if ($Thumbprint -notmatch '^[0-9A-Fa-f]{40}$') { Fail 'a 40-hex certificate thumbprint is required' }
            $store = if ($MachineStore) { 'Cert:\LocalMachine\My' } else { 'Cert:\CurrentUser\My' }
            $cert = Get-Item -LiteralPath (Join-Path $store $Thumbprint) -ErrorAction SilentlyContinue
            if (-not $cert -or -not $cert.HasPrivateKey) { Fail "the signing certificate is not provisioned in $store with a private key" }
            if ($cert.Subject -eq $cert.Issuer) { Fail 'self-signed certificates are never used for release signing' }
            if (-not ($cert.EnhancedKeyUsageList | Where-Object { $_.ObjectId -eq $CodeSigningEku })) { Fail 'the certificate has no code-signing EKU' }
            $backendArgs = @('/sha1', $Thumbprint.ToUpperInvariant()) + $(if ($MachineStore) { @('/sm') } else { @() })
        }
    }
    foreach ($file in $Files) {
        $target = Resolve-Target $file $WorkRoot
        & $signtool @common @backendArgs $target
        if ($LASTEXITCODE -ne 0) { Fail "SignTool failed ($LASTEXITCODE) for $(Split-Path -Leaf $target)" }
    }
}

# The only context in which release credentials may be used: a push of a
# vMAJOR.MINOR.PATCH[-pre] tag whose top-level workflow is this repository's
# ci.yml at that same tag, building exactly that version.
function Assert-MetroReleaseContext([string] $Version) {
    if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.\-]{1,40})?$') { Fail 'invalid release version' }
    $ref = "refs/tags/v$Version"
    if ($env:GITHUB_EVENT_NAME -ne 'push' -or $env:GITHUB_REF_TYPE -ne 'tag' -or $env:GITHUB_REF -cne $ref -or
        $env:GITHUB_REF_PROTECTED -ne 'true' -or
        $env:GITHUB_WORKFLOW_REF -cne "$env:GITHUB_REPOSITORY/.github/workflows/ci.yml@$ref") {
        Fail "release signing runs only for a pushed, protected tag v$Version from .github/workflows/ci.yml"
    }
}

function Install-MetroArtifactSigningClient([string] $Version, [string] $Sha256, [string] $Dest) {
    if ($Version -notmatch '^\d+\.\d+\.\d+$' -or $Sha256 -notmatch '^[0-9a-f]{64}$') {
        Fail 'METRO_ARTIFACT_SIGNING_CLIENT_VERSION and _SHA256 (lowercase hex of the .nupkg) must be set'
    }
    if (Test-Path -LiteralPath $Dest) { Fail "$Dest already exists" }
    New-Item -ItemType Directory -Path $Dest | Out-Null
    $pkg = Join-Path $Dest 'client.nupkg'
    $url = "https://api.nuget.org/v3-flatcontainer/microsoft.artifactsigning.client/$Version/microsoft.artifactsigning.client.$Version.nupkg"
    Invoke-WebRequest -Uri $url -OutFile $pkg -MaximumRetryCount 3 -RetryIntervalSec 5
    if ((Get-FileHash -LiteralPath $pkg -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Sha256) { Fail 'Artifact Signing client package checksum mismatch' }
    Expand-Archive -LiteralPath $pkg -DestinationPath (Join-Path $Dest 'pkg')
    $dlib = @(Get-ChildItem -LiteralPath (Join-Path $Dest 'pkg') -Recurse -File -Filter 'Azure.CodeSigning.Dlib.dll' |
        Where-Object { $_.Directory.Name -eq 'x64' })
    if ($dlib.Count -ne 1) { Fail 'expected exactly one x64 Azure.CodeSigning.Dlib.dll in the client package' }
    $sig = Get-AuthenticodeSignature -LiteralPath $dlib[0].FullName
    if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch '(^|, )O=Microsoft Corporation(,|$)') {
        Fail 'the Artifact Signing dlib is not a valid Microsoft-signed binary'
    }
    if (-not (& dotnet --list-runtimes 2>$null | Where-Object { $_ -match '^Microsoft\.NETCore\.App 8\.' })) {
        Fail 'the Artifact Signing dlib needs the .NET 8 runtime'
    }
    return $dlib[0].FullName
}

# GitHub OIDC token for Entra workload identity federation, written to a file
# that only lives for the signing call. The value is masked and never printed.
function New-GitHubOidcTokenFile([string] $Path) {
    if (-not $env:ACTIONS_ID_TOKEN_REQUEST_URL -or -not $env:ACTIONS_ID_TOKEN_REQUEST_TOKEN) {
        Fail 'no GitHub OIDC token is available: the tag call must grant id-token: write'
    }
    $uri = $env:ACTIONS_ID_TOKEN_REQUEST_URL + '&audience=api://AzureADTokenExchange'
    $token = (Invoke-RestMethod -Uri $uri -Headers @{ Authorization = "bearer $env:ACTIONS_ID_TOKEN_REQUEST_TOKEN" }).value
    if (-not $token) { Fail 'empty OIDC token' }
    Write-Host "::add-mask::$token"
    [IO.File]::WriteAllText($Path, $token)
}

# Signs with the backend this repository configured (vars only; no stored secret).
function Invoke-MetroReleaseSigning {
    [CmdletBinding()]
    param([Parameter(Mandatory)] [string] $Version, [Parameter(Mandatory)] [string] $WorkRoot, [Parameter(Mandatory)] [string[]] $Files)
    Assert-MetroReleaseContext $Version
    if (-not $env:METRO_WINDOWS_PUBLISHER_CN) { Fail 'METRO_WINDOWS_PUBLISHER_CN must be set' }
    switch ($env:METRO_WINDOWS_SIGNING_BACKEND) {
        'azure' {
            $endpoint = $env:METRO_ARTIFACT_SIGNING_ENDPOINT
            if ($endpoint -notmatch '^https://[a-z0-9]+\.codesigning\.azure\.net/?$') { Fail 'METRO_ARTIFACT_SIGNING_ENDPOINT must be a regional https://<region>.codesigning.azure.net endpoint' }
            foreach ($n in 'METRO_ARTIFACT_SIGNING_ACCOUNT', 'METRO_ARTIFACT_SIGNING_PROFILE') {
                if ("$([Environment]::GetEnvironmentVariable($n))" -notmatch '^[A-Za-z0-9][A-Za-z0-9\-]{1,62}$') { Fail "$n must be set" }
            }
            foreach ($n in 'METRO_AZURE_CLIENT_ID', 'METRO_AZURE_TENANT_ID') {
                if ("$([Environment]::GetEnvironmentVariable($n))" -notmatch '^[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$') { Fail "$n must be a GUID" }
            }
            $temp = Join-Path $env:RUNNER_TEMP ('artifact-signing-' + [Guid]::NewGuid().ToString('N'))
            $dlib = Install-MetroArtifactSigningClient $env:METRO_ARTIFACT_SIGNING_CLIENT_VERSION $env:METRO_ARTIFACT_SIGNING_CLIENT_SHA256 (Join-Path $temp 'client')
            $metadata = Join-Path $temp 'metadata.json'
            $tokenFile = Join-Path $temp 'oidc-token'
            @{
                Endpoint = $endpoint; CodeSigningAccountName = $env:METRO_ARTIFACT_SIGNING_ACCOUNT
                CertificateProfileName = $env:METRO_ARTIFACT_SIGNING_PROFILE; CorrelationId = "metro-$Version"
                ExcludeCredentials = @('EnvironmentCredential', 'ManagedIdentityCredential', 'SharedTokenCacheCredential',
                    'VisualStudioCredential', 'VisualStudioCodeCredential', 'AzureCliCredential', 'AzurePowerShellCredential',
                    'AzureDeveloperCliCredential', 'InteractiveBrowserCredential')
            } | ConvertTo-Json | Set-Content -LiteralPath $metadata -Encoding utf8
            try {
                New-GitHubOidcTokenFile $tokenFile
                $env:AZURE_CLIENT_ID = $env:METRO_AZURE_CLIENT_ID
                $env:AZURE_TENANT_ID = $env:METRO_AZURE_TENANT_ID
                $env:AZURE_FEDERATED_TOKEN_FILE = $tokenFile
                Invoke-MetroSigning -Backend azure -WorkRoot $WorkRoot -Files $Files -Dlib $dlib -MetadataFile $metadata `
                    -TimestampUrl 'http://timestamp.acs.microsoft.com'
            } finally {
                Remove-Item -LiteralPath $tokenFile -Force -ErrorAction SilentlyContinue
                Remove-Item env:AZURE_FEDERATED_TOKEN_FILE, env:AZURE_CLIENT_ID, env:AZURE_TENANT_ID -ErrorAction SilentlyContinue
            }
        }
        'kms' {
            $store = $env:METRO_WINDOWS_SIGNING_CERT_STORE
            if ($store -notin 'CurrentUser', 'LocalMachine') { Fail 'METRO_WINDOWS_SIGNING_CERT_STORE must be CurrentUser or LocalMachine' }
            Invoke-MetroSigning -Backend kms -WorkRoot $WorkRoot -Files $Files -Thumbprint $env:METRO_WINDOWS_SIGNING_CERT_SHA1 `
                -MachineStore:($store -eq 'LocalMachine') -TimestampUrl $env:METRO_WINDOWS_TIMESTAMP_URL
        }
        default { Fail 'METRO_WINDOWS_SIGNING_BACKEND must be azure or kms' }
    }
}

function Get-SubjectPart($Certificate, [string] $Key) {
    $parts = $Certificate.SubjectName.Format($true) -split "`r?`n" | Where-Object { $_ -like "$Key=*" }
    if (@($parts).Count -ne 1) { return $null }
    return ([string] $parts).Substring($Key.Length + 1).Trim('"')
}

# Windows' own trust decision for one signed file: SignTool's default
# Authenticode policy across all signatures with timestamps required, then the
# signer and timestamp certificates as Windows reports them.
function Test-MetroOsTrust {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [string] $PublisherCN,
        [string] $PublisherOrg,
        [string] $SignTool = (Get-MetroSignTool)
    )
    if (-not $PublisherCN) { Fail 'the expected publisher CN is required' }
    $name = Split-Path -Leaf $Path
    $out = & $SignTool verify /pa /all /tw $Path 2>&1
    # 0 = valid; 2 = valid with warnings (e.g. not timestamped), which is a failure here.
    if ($LASTEXITCODE -ne 0) { Fail "SignTool rejects ${name} (exit $LASTEXITCODE): $($out -join ' ')" }
    $sig = Get-AuthenticodeSignature -LiteralPath $Path
    if ($sig.Status -ne 'Valid' -or $sig.SignatureType -ne 'Authenticode') { Fail "${name}: Windows reports $($sig.Status)" }
    $signer = $sig.SignerCertificate
    if (-not $signer -or $signer.Subject -eq $signer.Issuer) { Fail "${name}: missing or self-signed signer" }
    if (-not ($signer.EnhancedKeyUsageList | Where-Object { $_.ObjectId -eq $CodeSigningEku })) { Fail "${name}: signer has no code-signing EKU" }
    if ((Get-SubjectPart $signer 'CN') -cne $PublisherCN) { Fail "${name}: publisher CN is not the expected Metro publisher" }
    if ($PublisherOrg -and (Get-SubjectPart $signer 'O') -cne $PublisherOrg) { Fail "${name}: publisher O is not the expected Metro publisher" }
    $tsa = $sig.TimeStamperCertificate
    if (-not $tsa) { Fail "${name}: no trusted timestamp" }
    if (-not ($tsa.EnhancedKeyUsageList | Where-Object { $_.ObjectId -eq $TimeStampingEku })) { Fail "${name}: timestamp signer has no time-stamping EKU" }
    Write-Host "${name}: trusted by Windows; publisher CN matches; timestamped by $($tsa.GetNameInfo('SimpleName', $false))"
}

# Expects Test-MetroOsTrust to reject the file; returns the reason.
function Assert-MetroRejected {
    param([Parameter(Mandatory)] [string] $Path, [Parameter(Mandatory)] [string] $PublisherCN, [string] $SignTool = (Get-MetroSignTool))
    try { Test-MetroOsTrust -Path $Path -PublisherCN $PublisherCN -SignTool $SignTool }
    catch { Write-Host "rejected as expected: $(Split-Path -Leaf $Path): $($_.Exception.Message)"; return }
    Fail "$(Split-Path -Leaf $Path) was accepted but must be rejected"
}

# Flip one byte in a copy, away from the signature, so its hash no longer matches.
function New-TamperedCopy([string] $Path, [string] $Dest) {
    Copy-Item -LiteralPath $Path -Destination $Dest
    $bytes = [IO.File]::ReadAllBytes($Dest)
    if ($Path -like '*.ps1') {
        $i = [Text.Encoding]::ASCII.GetString($bytes).IndexOf('#>')
    } else {
        $i = [int] ($bytes.Length / 3)
    }
    if ($i -le 0) { Fail "cannot tamper with $Path" }
    $bytes[$i] = $bytes[$i] -bxor 0x20
    [IO.File]::WriteAllBytes($Dest, $bytes)
    return $Dest
}

# ---------------------------------------------------------------- MSI checks

function Invoke-Com($Object, [string] $Name, [object[]] $Arguments = @()) {
    $Object.GetType().InvokeMember($Name, [Reflection.BindingFlags]::InvokeMethod, $null, $Object, $Arguments)
}
function Get-ComProperty($Object, [string] $Name, [object[]] $Arguments = @()) {
    $Object.GetType().InvokeMember($Name, [Reflection.BindingFlags]::GetProperty, $null, $Object, $Arguments)
}

function Get-MsiRows($Database, [string] $Query, [int] $Columns) {
    $view = Invoke-Com $Database 'OpenView' @($Query)
    Invoke-Com $view 'Execute' | Out-Null
    $rows = @()
    while ($true) {
        $record = Invoke-Com $view 'Fetch'
        if ($null -eq $record) { break }
        $rows += , @(1..$Columns | ForEach-Object { Get-ComProperty $record 'StringData' @($_) })
    }
    Invoke-Com $view 'Close' | Out-Null
    return , $rows
}

# Structural rules for the MSI: only property, directory and error custom
# actions (types 51, 35, 19) exist, so no DLL, EXE or script action can run.
function Test-MetroMsiStructure {
    [CmdletBinding()]
    param([Parameter(Mandatory)] [string] $Msi, [Parameter(Mandatory)] [string] $Version)
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $db = Invoke-Com $installer 'OpenDatabase' @($Msi, 0)
    $tables = (Get-MsiRows $db 'SELECT `Name` FROM `_Tables`' 1) | ForEach-Object { $_[0] }
    foreach ($t in 'MsiEmbeddedUI', 'MsiEmbeddedChainer', 'ServiceInstall', 'ServiceControl', 'SelfReg', 'MsiDigitalSignature') {
        if ($tables -contains $t -and (Get-MsiRows $db "SELECT * FROM ``$t``" 1).Count -gt 0) { Fail "MSI has rows in $t" }
    }
    $props = @{}
    foreach ($r in (Get-MsiRows $db 'SELECT `Property`, `Value` FROM `Property`' 2)) { $props[$r[0]] = $r[1] }
    # The MSI lane's SemVer -> ProductVersion mapping (MAJOR.MINOR.PATCH*100+rank).
    $mapped = & python -I (Join-Path $PSScriptRoot '..\windows-msi\build_msi.py') version $Version
    if ($LASTEXITCODE -ne 0 -or -not $mapped) { Fail "cannot map $Version to a ProductVersion" }
    $expectedVersion = ([string] $mapped).Split(' ')[0]
    if ($props['ProductVersion'] -ne $expectedVersion) { Fail "MSI ProductVersion $($props['ProductVersion']) is not $expectedVersion" }
    if (-not $props['ProductCode'] -or -not $props['UpgradeCode']) { Fail 'MSI has no ProductCode or UpgradeCode' }
    if ((Get-MsiRows $db "SELECT ``Directory`` FROM ``Directory`` WHERE ``Directory``='INSTALLDIR'" 1).Count -ne 1) {
        Fail 'MSI has no INSTALLDIR directory'
    }
    $actions = @{}
    if ($tables -contains 'CustomAction') {
        foreach ($r in (Get-MsiRows $db 'SELECT `Action`, `Type`, `Source` FROM `CustomAction`' 3)) {
            $actions[$r[0]] = @{ Type = [int] $r[1]; Source = $r[2] }
        }
    }
    $safe = 19, 35, 51
    foreach ($name in $actions.Keys) {
        if (-not ($safe -contains ($actions[$name].Type -band 0x3F))) { Fail "MSI custom action $name (type $($actions[$name].Type)) is not allowed" }
    }
    [Runtime.InteropServices.Marshal]::FinalReleaseComObject($db) | Out-Null
    return @{ ProductCode = $props['ProductCode'] }
}

# A copy with one extra Property row: every hashed stream stays, the content changes.
function New-TamperedMsiCopy([string] $Path, [string] $Dest) {
    Copy-Item -LiteralPath $Path -Destination $Dest
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $db = Invoke-Com $installer 'OpenDatabase' @($Dest, 1)
    $view = Invoke-Com $db 'OpenView' @("INSERT INTO ``Property`` (``Property``, ``Value``) VALUES ('MetroTamperCheck', '1')")
    Invoke-Com $view 'Execute' | Out-Null
    Invoke-Com $view 'Close' | Out-Null
    Invoke-Com $db 'Commit' | Out-Null
    [Runtime.InteropServices.Marshal]::FinalReleaseComObject($db) | Out-Null
    return $Dest
}

function Get-MetroMsiProductState([string] $ProductCode) {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    return [int] (Get-ComProperty $installer 'ProductState' @($ProductCode))
}

function Invoke-Msiexec([string[]] $Arguments, [string] $Log) {
    $p = Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" -ArgumentList ($Arguments + @('/qn', '/norestart', '/L*v', "`"$Log`"")) -Wait -PassThru
    if ($p.ExitCode -notin 0, 3010) {
        Write-Host (Get-Content -LiteralPath $Log -Tail 60 -ErrorAction SilentlyContinue | Out-String)
        Fail "msiexec $($Arguments[0]) failed with exit code $($p.ExitCode)"
    }
}

# ---------------------------------------------------------------- shared CI steps

# Native empty arguments are not passed reliably, so publisher flags are added only when set.
function Get-MetroPublisherArgs([string] $PublisherCN, [string] $PublisherOrg) {
    $a = @()
    if ($PublisherCN) { $a += @('--publisher-cn', $PublisherCN) }
    if ($PublisherOrg) { $a += @('--publisher-org', $PublisherOrg) }
    return , $a
}

function Invoke-WinSign {
    $out = & python -I (Join-Path $PSScriptRoot 'winsign.py') @args
    if ($LASTEXITCODE -ne 0) { Fail "winsign $($args[0]) failed" }
    return $out
}

# Windows-side checks of an MSI built by packaging/windows-msi (on Linux):
# signature state, table rules, and an administrative image that holds exactly
# the release tree (plus the optional empty local-runner marker).
function Test-MetroMsiCandidate {
    [CmdletBinding()]
    param([Parameter(Mandatory)] [string] $Msi, [Parameter(Mandatory)] [string] $Tree, [Parameter(Mandatory)] [string] $Version,
          [Parameter(Mandatory)] [ValidateSet('official', 'preview')] [string] $Mode,
          [Parameter(Mandatory)] [ValidateSet('signed', 'unsigned')] [string] $Expect, [Parameter(Mandatory)] [string] $LogDir,
          [string] $PublisherCN = '', [string] $PublisherOrg = '')
    Invoke-WinSign msi-state --msi $Msi --expect $Expect | Write-Host
    Test-MetroMsiStructure -Msi $Msi -Version $Version | Out-Null
    $image = Join-Path ([IO.Path]::GetTempPath()) ('metro admin image ' + [Guid]::NewGuid().ToString('N'))
    Invoke-Msiexec @('/a', "`"$Msi`"", "TARGETDIR=`"$image`"") (Join-Path $LogDir 'msi-admin.log')
    $pub = Get-MetroPublisherArgs $PublisherCN $PublisherOrg
    Invoke-WinSign verify-admin-image --image $image --manifest-from $Tree --version $Version --mode $Mode @pub | Write-Host
    Remove-Item -LiteralPath $image -Recurse -Force
}

# Official only: Windows' own trust decision (fresh hosted VM, no certificate
# added) for every signed file, then tampered and unsigned copies are rejected.
function Test-MetroOfficialTrust {
    [CmdletBinding()]
    param([Parameter(Mandatory)] [string] $Tree, [Parameter(Mandatory)] [string] $Dist, [Parameter(Mandatory)] [string] $MsiName,
          [Parameter(Mandatory)] [string] $SeedRoot, [Parameter(Mandatory)] [string] $SourceScripts,
          [Parameter(Mandatory)] [string] $PublisherCN, [string] $PublisherOrg = '')
    $tool = Get-MetroSignTool
    $exes = 'Metro.exe', 'metro-server.exe', 'metro-tui.exe', 'ui\Metro.exe'
    $targets = @($exes | ForEach-Object { Join-Path $Tree $_ }) +
        @('install-metro-windows.ps1', 'metro-runner-container.ps1', $MsiName | ForEach-Object { Join-Path $Dist $_ })
    foreach ($t in $targets) { Test-MetroOsTrust -Path $t -PublisherCN $PublisherCN -PublisherOrg $PublisherOrg -SignTool $tool }
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ('metro-tamper-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $scratch | Out-Null
    Assert-MetroRejected (New-TamperedCopy (Join-Path $Tree 'Metro.exe') "$scratch\Metro.exe") $PublisherCN $tool
    Assert-MetroRejected (New-TamperedCopy (Join-Path $Tree 'metro-tui.exe') "$scratch\metro-tui.exe") $PublisherCN $tool
    Assert-MetroRejected (New-TamperedCopy (Join-Path $Dist 'install-metro-windows.ps1') "$scratch\install-metro-windows.ps1") $PublisherCN $tool
    Assert-MetroRejected (New-TamperedMsiCopy (Join-Path $Dist $MsiName) "$scratch\tampered.msi") $PublisherCN $tool
    foreach ($f in $exes) { Assert-MetroRejected (Join-Path $SeedRoot $f) $PublisherCN $tool }
    Assert-MetroRejected (Join-Path $SourceScripts 'install-metro-windows.ps1') $PublisherCN $tool
    & python -I (Join-Path $PSScriptRoot 'winsign.py') inspect (Join-Path $SeedRoot 'Metro.exe') --publisher-cn $PublisherCN
    if ($LASTEXITCODE -eq 0) { Fail 'the unsigned seed launcher was accepted' }
    $global:LASTEXITCODE = 0
    Remove-Item -LiteralPath $scratch -Recurse -Force
}

# Installs the MSI into a chosen folder with spaces, checks the installed
# payload against the release, starts the installed server, then uninstalls.
function Test-MetroMsiInstall {
    [CmdletBinding()]
    param([Parameter(Mandatory)] [string] $Msi, [Parameter(Mandatory)] [string] $Tree, [Parameter(Mandatory)] [string] $Version,
          [Parameter(Mandatory)] [ValidateSet('official', 'preview')] [string] $Mode, [Parameter(Mandatory)] [string] $LogDir,
          [string] $PublisherCN = '', [string] $PublisherOrg = '')
    $info = Test-MetroMsiStructure -Msi $Msi -Version $Version
    $dir = Join-Path ([IO.Path]::GetTempPath()) ('Chosen Metro Folder ' + [Guid]::NewGuid().ToString('N') + '\Metro custom')
    if ((Get-MetroMsiProductState $info.ProductCode) -eq 5) { Fail 'Metro is already installed on this machine' }
    Invoke-Msiexec @('/i', "`"$Msi`"", "INSTALLDIR=`"$dir`"") (Join-Path $LogDir 'msi-install.log')
    if ((Get-MetroMsiProductState $info.ProductCode) -ne 5) { Fail 'the MSI did not register as installed' }
    $pub = Get-MetroPublisherArgs $PublisherCN $PublisherOrg
    Invoke-WinSign verify-installed --root $dir --manifest-from $Tree --version $Version --mode $Mode --allow-marker @pub | Write-Host
    if (-not (Test-Path -LiteralPath (Join-Path $dir 'enable-local-runner') -PathType Leaf)) { Fail 'the default install did not create the local-runner marker' }
    if ($Mode -eq 'official') {
        $tool = Get-MetroSignTool
        foreach ($f in 'Metro.exe', 'metro-server.exe', 'metro-tui.exe', 'ui\Metro.exe') {
            Test-MetroOsTrust -Path (Join-Path $dir $f) -PublisherCN $PublisherCN -PublisherOrg $PublisherOrg -SignTool $tool
        }
    }
    # The installed server answers on loopback (throwaway dev user, never printed).
    $work = Join-Path $LogDir 'installed-server'
    New-Item -ItemType Directory -Path $work | Out-Null
    $bytes = [byte[]]::new(24); [Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
    $saved = @{}
    foreach ($k in 'METRO_BIND', 'METRO_DB', 'METRO_DEV_USERS') { $saved[$k] = [Environment]::GetEnvironmentVariable($k) }
    $env:METRO_BIND = '127.0.0.1:18797'
    $env:METRO_DB = Join-Path $work 'metro.sqlite3'
    $env:METRO_DEV_USERS = '{"ci":"' + (($bytes | ForEach-Object { $_.ToString('x2') }) -join '') + '"}'
    try {
        $p = Start-Process -FilePath (Join-Path $dir 'metro-server.exe') -WorkingDirectory $work -PassThru `
            -RedirectStandardOutput "$work\out.log" -RedirectStandardError "$work\err.log"
    } finally { foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) } }
    try {
        $ok = $false
        for ($i = 0; $i -lt 30 -and -not $ok; $i++) {
            Start-Sleep -Seconds 1
            if ($p.HasExited) { Fail "installed metro-server exited early ($($p.ExitCode))" }
            try { $ok = $null -ne (Invoke-RestMethod 'http://127.0.0.1:18797/healthz' -NoProxy -TimeoutSec 2) } catch { }
        }
        if (-not $ok) { Fail 'installed metro-server did not answer /healthz' }
    } finally { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force; $p.WaitForExit() } }
    Invoke-Msiexec @('/x', "`"$Msi`"") (Join-Path $LogDir 'msi-uninstall.log')
    if ((Get-MetroMsiProductState $info.ProductCode) -eq 5) { Fail 'the MSI is still registered after uninstall' }
    if ((Test-Path -LiteralPath $dir) -and (Get-ChildItem -LiteralPath $dir -Recurse -Force)) { Fail 'uninstall left files in the install folder' }
    Write-Host "MSI installed into a chosen folder, matched the release, ran the server and uninstalled cleanly"
}

# Preview only. A throwaway self-signed certificate signs scratch copies so the
# verifiers are exercised on real SignTool output; it is never trusted, never
# timestamped, never shipped, and removed afterwards.
function Invoke-MetroVerifierSelfTest {
    [CmdletBinding()]
    param([Parameter(Mandatory)] [string] $Tree, [Parameter(Mandatory)] [string] $Msi, [Parameter(Mandatory)] [string] $SourceScripts)
    if ($env:GITHUB_REF_TYPE -eq 'tag') { Fail 'the throwaway-certificate self-test never runs for tags' }
    $tool = Get-MetroSignTool
    $subject = 'Metro CI throwaway self-test (not a release signer)'
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ('metro-self-test-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $scratch | Out-Null
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=$subject" -CertStoreLocation Cert:\CurrentUser\My `
        -KeyExportPolicy NonExportable -NotAfter (Get-Date).AddHours(2)
    try {
        $pairs = @(
            @{ In = (Join-Path $Tree 'Metro.exe'); Out = "$scratch\Metro.exe" },
            @{ In = (Join-Path $Tree 'ui\Metro.exe'); Out = "$scratch\renderer.exe" },
            @{ In = (Join-Path $SourceScripts 'install-metro-windows.ps1'); Out = "$scratch\install-metro-windows.ps1" },
            @{ In = $Msi; Out = "$scratch\preview.msi" }
        )
        foreach ($p in $pairs) {
            Copy-Item -LiteralPath $p.In -Destination $p.Out
            & $tool sign /fd SHA256 /sha1 $cert.Thumbprint $p.Out
            if ($LASTEXITCODE -ne 0) { Fail "self-test signing failed for $($p.Out)" }
        }
        # Real SignTool output changes only signature bytes; the renderer's code stays the pinned stock code.
        foreach ($p in $pairs[0..2]) { Invoke-WinSign delta --unsigned $p.In --signed $p.Out | Write-Host }
        Invoke-WinSign msi-state --msi "$scratch\preview.msi" --expect signed | Write-Host
        foreach ($p in $pairs) {
            Assert-MetroRejected $p.Out $subject $tool
            & python -I (Join-Path $PSScriptRoot 'winsign.py') inspect $p.Out --publisher-cn $subject
            if ($LASTEXITCODE -eq 0) { Fail "self-signed $($p.Out) was accepted" }
        }
        $t = New-TamperedCopy "$scratch\Metro.exe" "$scratch\tampered.exe"
        if ((Get-AuthenticodeSignature -LiteralPath $t).Status -ne 'HashMismatch') { Fail 'a tampered EXE was not a hash mismatch' }
        $t = New-TamperedMsiCopy "$scratch\preview.msi" "$scratch\tampered.msi"
        if ((Get-AuthenticodeSignature -LiteralPath $t).Status -ne 'HashMismatch') { Fail 'a tampered MSI was not a hash mismatch' }
        $global:LASTEXITCODE = 0
        Write-Host 'verifier self-test passed (throwaway certificate rejected; tampering detected; signature-only deltas)'
    } finally {
        Remove-Item -LiteralPath "Cert:\CurrentUser\My\$($cert.Thumbprint)" -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Export-ModuleMember -Function Get-MetroPublisherArgs, Invoke-WinSign, Get-MetroSignTool, Invoke-MetroSigning, Invoke-MetroReleaseSigning, Assert-MetroReleaseContext,
    Test-MetroOsTrust, Assert-MetroRejected, New-TamperedCopy, New-TamperedMsiCopy, Test-MetroMsiStructure,
    Get-MetroMsiProductState, Invoke-Msiexec, Test-MetroMsiCandidate, Test-MetroOfficialTrust, Test-MetroMsiInstall,
    Invoke-MetroVerifierSelfTest
