<#
.SYNOPSIS
  Metro runner in a container on Windows (Docker Desktop, Linux containers).

.DESCRIPTION
  Runs the official prebuilt Linux runner image. The runner only makes
  outbound connections; no port is published. This script never creates,
  rotates or prints a runner token: create one in Metro, save it to a file
  only you can read, and pass its path.

  -ServerUrl must be https://..., or http://127.0.0.1:PORT for a Metro
  server on this computer (stable port). Loopback needs Docker Desktop 4.34+
  with Settings > Resources > Network > Enable host networking.
  The named volume (default metro-runner-workspace) is mounted at /workspace;
  the runner keeps each Space's files in spaces/<space-id>/. Remove never
  deletes it.

.EXAMPLE
  .\metro-runner-container.ps1 -Action Setup -ServerUrl https://metro.example -TokenFile $HOME\metro-runner.token -Version 0.2.0
  .\metro-runner-container.ps1 -Action Start
#>
[CmdletBinding()]
param(
    [ValidateSet('Setup', 'Start', 'Stop', 'Restart', 'Status', 'Logs', 'Update', 'Remove')]
    [Parameter(Mandatory = $true)] [string] $Action,
    [string] $ServerUrl,
    [string] $TokenFile,
    [string] $Version,
    [string] $Image,
    [string] $Volume,
    [string] $Name,
    [switch] $PurgeConfig
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$Prog = 'metro-runner-container'
$Secret = '/run/secrets/metro_runner_token'
$DefaultRepo = 'ghcr.io/wnzn/metro-runner'
$OnWindows = [System.Environment]::OSVersion.Platform -eq 'Win32NT'
if ($env:METRO_RUNNER_CONFIG_DIR) { $ConfigDir = $env:METRO_RUNNER_CONFIG_DIR }
else { $ConfigDir = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Metro\Runner' }
$Config = Join-Path $ConfigDir 'container.json'
$Docker = if ($env:METRO_DOCKER_BIN) { $env:METRO_DOCKER_BIN } else { 'docker' }

function Fail([string] $Message) { throw "${Prog}: $Message" }

# Plain http only to an exact loopback literal (localhost, 127.0.0.1 or [::1])
# with an explicit numeric port 1-65535, then an optional path.
function Test-LoopbackHttp([string] $Url) {
    if (-not ($Url -cmatch '^http://(localhost|127\.0\.0\.1|\[::1\]):([0-9]{1,5})(/[^?#@]*)?$')) { return $false }
    $port = [int] $Matches[2]
    return $port -ge 1 -and $port -le 65535
}

function Test-Url([string] $Url) {
    if ($Url -match '[\s?#@"`$\\,]') { Fail '-ServerUrl must be a plain URL without spaces, credentials, query or fragment' }
    if ($Url -cmatch '^https://[^/:]') {
        $u = $null
        if (-not [Uri]::TryCreate($Url, [UriKind]::Absolute, [ref] $u) -or -not $u.Host -or $u.UserInfo) { Fail "invalid -ServerUrl: $Url" }
        return
    }
    if ($Url -cmatch '^http://') {
        if (Test-LoopbackHttp $Url) { return }
        Fail 'plain http to a non-loopback host would send the runner token unencrypted; use https:// (http:// only to 127.0.0.1:PORT, localhost:PORT or [::1]:PORT)'
    }
    Fail '-ServerUrl must start with https:// (or http:// for loopback)'
}

# Never reads the token. On Windows, the file must be owned by you and not
# readable by Everyone, Users or Authenticated Users.
function Test-TokenFile([string] $Path) {
    if (-not [System.IO.Path]::IsPathRooted($Path)) { Fail 'token file path must be absolute' }
    if ($Path.Contains(',')) { Fail 'token file path must not contain commas' }
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { Fail "token file not found: $Path" }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) { Fail "token file must not be a link: $Path" }
    if ($item.Length -eq 0) { Fail "token file is empty: $Path" }
    if ($OnWindows) {
        $acl = Get-Acl -LiteralPath $Path
        $me = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
        $owner = (New-Object System.Security.Principal.NTAccount($acl.Owner)).Translate([System.Security.Principal.SecurityIdentifier])
        if ($owner -ne $me) { Fail "token file must be owned by you: $Path" }
        $broad = 'S-1-1-0', 'S-1-5-32-545', 'S-1-5-11'  # Everyone, Users, Authenticated Users
        foreach ($rule in $acl.Access) {
            $sid = $rule.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value
            if ($rule.AccessControlType -eq 'Allow' -and $broad -contains $sid) {
                Fail ("token file is readable by $($rule.IdentityReference). Restrict it to yourself, for example: " +
                      "icacls `"$Path`" /inheritance:r /grant:r `"$($env:USERNAME):R`"")
            }
        }
    }
}

function Test-Name([string] $What, [string] $Value) {
    if ($Value -notmatch '^[A-Za-z0-9][A-Za-z0-9_.-]{0,62}$') { Fail "invalid ${What}: $Value" }
}
function Test-Image([string] $Ref) {
    if ($Ref -notmatch '^[a-z0-9][a-z0-9._/:-]*(@sha256:[0-9a-f]{64})?$') { Fail "invalid image reference: $Ref" }
}

function Invoke-Engine {
    $out = & $Docker @args 2>&1
    $script:EngineExit = $LASTEXITCODE
    return $out
}

function Assert-Engine {
    if (-not (Get-Command $Docker -ErrorAction SilentlyContinue)) { Fail 'Docker Desktop is required (https://docs.docker.com/desktop/)' }
    Invoke-Engine info | Out-Null
    if ($EngineExit -ne 0) { Fail 'Docker is installed but not running; start Docker Desktop' }
    $os = (Invoke-Engine info --format '{{.OSType}}' | Out-String).Trim()
    if ($os -ne 'linux') { Fail 'Docker Desktop must use Linux containers (right-click the Docker icon: Switch to Linux containers)' }
}

function Read-Config {
    if (-not (Test-Path -LiteralPath $Config -PathType Leaf)) { Fail "not set up: run -Action Setup first" }
    $c = Get-Content -LiteralPath $Config -Raw | ConvertFrom-Json
    Test-Url $c.ServerUrl; Test-TokenFile $c.TokenFile; Test-Image $c.Image
    Test-Name 'container name' $c.Name; Test-Name 'volume' $c.Volume
    if ($c.Network -notin 'host', 'default') { Fail "$Config has an invalid network setting" }
    return $c
}

function Write-Config($c) {
    New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null
    $tmp = "$Config.new"
    $c | ConvertTo-Json | Set-Content -LiteralPath $tmp -Encoding UTF8
    Move-Item -LiteralPath $tmp -Destination $Config -Force
}

function Get-NetworkArgs($c) { if ($c.Network -eq 'host') { @('--network=host') } else { @() } }

function Get-TokenMount($c) { "type=bind,source=$($c.TokenFile),target=$Secret,readonly" }

function Confirm-Image([string] $Ref) {
    Invoke-Engine image inspect $Ref | Out-Null
    if ($EngineExit -ne 0) {
        Write-Host "Pulling $Ref ..."
        Invoke-Engine pull $Ref | Out-Null
        if ($EngineExit -ne 0) { Fail "cannot pull $Ref. Check the version and your network; Metro runner images are only available once that version is published." }
    }
}

function Invoke-Probe($c, [string] $Command) {
    $a = @('run', '--rm', '--pull=never') + (Get-NetworkArgs $c) + @('--read-only', '--cap-drop', 'ALL',
        '--security-opt', 'no-new-privileges:true', '--mount', (Get-TokenMount $c), '--entrypoint', '/bin/bash', $c.Image, '-c', $Command)
    Invoke-Engine @a | Out-Null
    return $EngineExit -eq 0
}

function Test-Exists($c) { Invoke-Engine container inspect $c.Name | Out-Null; return $EngineExit -eq 0 }

function Assert-Managed($c) {
    $label = (Invoke-Engine container inspect -f '{{index .Config.Labels "dev.metro.managed-by"}}' $c.Name | Out-String).Trim()
    if ($label -ne $Prog) { Fail "container $($c.Name) was not created by this script; refusing to change it" }
}

function Start-Runner($c) {
    if (Test-Exists $c) {
        $running = (Invoke-Engine container inspect -f '{{.State.Running}}' $c.Name | Out-String).Trim()
        if ($running -eq 'true') { Write-Host "$($c.Name) is already running."; return }
        Invoke-Engine start $c.Name | Out-Null
        if ($EngineExit -ne 0) { Fail "could not start $($c.Name)" }
        Write-Host "Started $($c.Name)."; return
    }
    Confirm-Image $c.Image
    $script = 'IFS= read -r METRO_RUNNER_TOKEN < ' + $Secret + ' || [ -n "$METRO_RUNNER_TOKEN" ]; export METRO_RUNNER_TOKEN; exec /usr/local/bin/metro-runner'
    $a = @('run', '-d', '--name', $c.Name, '--restart', 'unless-stopped', '--pull=never') + (Get-NetworkArgs $c) + @(
        '--read-only', '--tmpfs', '/tmp:rw,noexec,nosuid,nodev,size=64m',
        '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges:true', '--pids-limit', '256',
        '--mount', "type=volume,source=$($c.Volume),target=/workspace",
        '--mount', (Get-TokenMount $c),
        '--env', "METRO_SERVER_URL=$($c.ServerUrl)",
        '--label', "dev.metro.managed-by=$Prog",
        '--entrypoint', '/bin/sh', $c.Image, '-c', $script)
    Invoke-Engine @a | Out-Null
    if ($EngineExit -ne 0) { Fail 'could not start the runner container' }
    Write-Host "Started $($c.Name) ($($c.Image)). See its log with: -Action Logs"
}

function Remove-Runner($c) {
    if (-not (Test-Exists $c)) { return }
    Assert-Managed $c
    Invoke-Engine rm -f $c.Name | Out-Null
}

function Get-ImageRef {
    if ($Image) { Test-Image $Image; return $Image }
    if (-not $Version) { Fail 'pass -Version X.Y.Z or -Image REF' }
    $v = $Version.TrimStart('v')
    if ($v -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') { Fail "invalid -Version: $Version" }
    return "${DefaultRepo}:v$v"
}

try {
    switch ($Action) {
        'Setup' {
            if (-not $ServerUrl -or -not $TokenFile) { Fail 'Setup needs -ServerUrl and -TokenFile' }
            $url = $ServerUrl.TrimEnd('/')
            Test-Url $url
            $tokenPath = (Resolve-Path -LiteralPath $TokenFile).ProviderPath
            Test-TokenFile $tokenPath
            $c = [pscustomobject]@{
                Image = (Get-ImageRef); ServerUrl = $url; TokenFile = $tokenPath
                Name = $(if ($Name) { $Name } else { 'metro-runner' })
                Volume = $(if ($Volume) { $Volume } else { 'metro-runner-workspace' })
                Network = $(if (Test-LoopbackHttp $url) { 'host' } else { 'default' })
            }
            Test-Name 'container name' $c.Name; Test-Name 'volume' $c.Volume
            Assert-Engine
            Confirm-Image $c.Image
            Invoke-Engine volume inspect $c.Volume | Out-Null
            if ($EngineExit -ne 0) { Invoke-Engine volume create $c.Volume | Out-Null; if ($EngineExit -ne 0) { Fail "cannot create volume $($c.Volume)" } }
            if (-not (Invoke-Probe $c "test -r $Secret && test -s $Secret")) {
                Fail "the container cannot read $tokenPath. Make sure Docker Desktop can share that folder (Settings > Resources > File sharing)."
            }
            if ($c.Network -eq 'host') {
                $u = [Uri] $url
                $h = if ($u.Host -eq '[::1]') { '::1' } else { '127.0.0.1' }
                if (-not (Invoke-Probe $c "exec 3<>/dev/tcp/$h/$($u.Port)")) {
                    Write-Warning "the container cannot reach $url right now. Make sure Metro is running on that port, and enable Docker Desktop's Settings > Resources > Network > Enable host networking (4.34+)."
                }
            }
            Write-Config $c
            Write-Host "Saved $Config (the token is not stored there)."
            Write-Host "Workspace volume: $($c.Volume) (kept by Stop, Update and Remove). Start it with: -Action Start"
        }
        'Start' { $c = Read-Config; Assert-Engine; Start-Runner $c }
        'Stop' {
            $c = Read-Config; Assert-Engine
            if (Test-Exists $c) { Assert-Managed $c; Invoke-Engine stop $c.Name | Out-Null; Write-Host "Stopped $($c.Name)." }
            else { Write-Host "$($c.Name) is not running." }
        }
        'Restart' { $c = Read-Config; Assert-Engine; Remove-Runner $c; Start-Runner $c }
        'Status' {
            $c = Read-Config; Assert-Engine
            if (Test-Exists $c) { Invoke-Engine container inspect -f "$($c.Name): {{.State.Status}} ({{.Config.Image}})" $c.Name }
            else { Write-Host "$($c.Name): not created" }
        }
        'Logs' { $c = Read-Config; Assert-Engine; Invoke-Engine logs --tail 200 $c.Name }
        'Update' {
            $c = Read-Config; $c.Image = Get-ImageRef; Assert-Engine
            Confirm-Image $c.Image; Remove-Runner $c; Write-Config $c; Start-Runner $c
        }
        'Remove' {
            if (Test-Path -LiteralPath $Config) {
                $c = Read-Config; Assert-Engine; Remove-Runner $c
                Write-Host "Removed container $($c.Name). Volume $($c.Volume) was kept (delete it yourself with: docker volume rm $($c.Volume))."
            }
            if ($PurgeConfig -and (Test-Path -LiteralPath $Config)) { Remove-Item -LiteralPath $Config -Force; Write-Host "Removed $Config" }
            Write-Host "Your token file was not changed. Revoke the runner's token in Metro if you no longer use it."
        }
    }
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
