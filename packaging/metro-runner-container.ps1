<#
.SYNOPSIS
  Metro runner in a container on Windows (Docker Desktop, Linux containers).

.DESCRIPTION
  Runs the official prebuilt Linux runner image. The runner only makes
  outbound connections; no port is published. This script never creates,
  rotates or prints a runner token: create one in Metro, save it to a file
  only you can read, and pass its path.

  -ServerUrl is https://..., or http:// to any host. For a Metro server on
  this computer use http://127.0.0.1:PORT (stable port); loopback needs
  Docker Desktop 4.34+ with Settings > Resources > Network > Enable host
  networking. Other hosts (a LAN address, Tailscale) need no host
  networking. Plain http to a host other than this computer or a Tailscale
  address (100.64.0.0/10, fd7a:115c:a1e0::/48, *.ts.net) works, with a
  warning: the runner token crosses the network unencrypted.
  The named volume (default metro-runner-workspace) is mounted at /workspace;
  the runner keeps each Space's files in spaces/<space-id>/. Remove never
  deletes it.

.EXAMPLE
  .\metro-runner-container.ps1 -Action Setup -ServerUrl https://metro.example -TokenFile $HOME\metro-runner.token -Version 0.2.0
  .\metro-runner-container.ps1 -Action Start

  Use the Authenticode-signed copy from the Metro release, and check it first:
  (Get-AuthenticodeSignature .\metro-runner-container.ps1).Status must be Valid.
  It runs under the RemoteSigned or AllSigned execution policy. Never bypass or
  turn off your execution policy for Metro.
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
# Fixed shell text for the container; settings reach it only as arguments or
# environment values, never inside the text. Same as RUNNER_ENTRY and
# PROBE_TCP in metro-runner-container.sh.
$RunnerEntry = 'if [ -n "${METRO_DIRECT_NO_PROXY:-}" ]; then NO_PROXY="${NO_PROXY-${no_proxy-}}"; NO_PROXY="${NO_PROXY:+$NO_PROXY,}$METRO_DIRECT_NO_PROXY"; no_proxy="$NO_PROXY"; export NO_PROXY no_proxy; fi; unset METRO_DIRECT_NO_PROXY; IFS= read -r METRO_RUNNER_TOKEN < /run/secrets/metro_runner_token || [ -n "$METRO_RUNNER_TOKEN" ]; export METRO_RUNNER_TOKEN; exec /usr/local/bin/metro-runner'
$ProbeTcp = 'exec 3<>"/dev/tcp/$1/$2"'
$ProbeToken = 'test -r "$1" && test -s "$1"'
$DefaultRepo = 'ghcr.io/wnzn/metro-runner'
$OnWindows = [System.Environment]::OSVersion.Platform -eq 'Win32NT'
if ($env:METRO_RUNNER_CONFIG_DIR) { $ConfigDir = $env:METRO_RUNNER_CONFIG_DIR }
else { $ConfigDir = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Metro\Runner' }
$Config = Join-Path $ConfigDir 'container.json'
$Docker = if ($env:METRO_DOCKER_BIN) { $env:METRO_DOCKER_BIN } else { 'docker' }

function Fail([string] $Message) { throw "${Prog}: $Message" }

# url-rule:begin (test_installers.py runs this block against
# crates/url-policy/testdata/host_vectors.json)
# Plain http works to any host. The host is parsed for real: an IPv6 address
# in brackets, a canonical dotted-quad IPv4 address, or a DNS name (labels of
# letters, digits and inner hyphens; one trailing dot allowed). Then:
# - loopback: only the exact literals localhost, 127.0.0.1 or [::1] with an
#   explicit port 1-65535; any other spelling of this computer (127.1,
#   LOCALHOST, 127.0.0.2, [0:0:0:0:0:0:0:1], ::ffff: forms, 0.0.0.0, [::]) is
#   refused as ambiguous;
# - Tailscale (100.64.0.0/10, fd7a:115c:a1e0::/48, names ending in .ts.net):
#   used silently, and always reached without a proxy (Get-DirectArgs);
# - any other host: allowed, with one warning, because the runner token
#   crosses the network unencrypted.
# Numbers must be a canonical dotted quad: resolvers read 100.077.0.1 as
# octal, 0x64 as hex and 100.64.1 as shorthand, so those are refused.
# Same algorithm as the http_class function of the shell installers.

function Test-Label([string] $Label) {
    return $Label.Length -ge 1 -and $Label.Length -le 63 -and $Label -cmatch '^[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?$'
}

# The groups of one side of '::' (or of a whole address), padded; $null when malformed.
function Get-V6Part([string] $Part) {
    if ($Part -eq '') { return , @() }
    if ($Part.StartsWith(':') -or $Part.EndsWith(':')) { return $null }
    $out = @()
    foreach ($g in $Part.Split(':')) {
        if ($g.Length -lt 1 -or $g.Length -gt 4) { return $null }
        $out += $g.PadLeft(4, '0')
    }
    return , $out
}

# The 8 groups of an IPv6 address, lowercase and padded; $null when malformed.
function Get-Ipv6Groups([string] $Text) {
    $t = $Text.ToLowerInvariant()
    if ($t -eq '' -or $t -notmatch '^[0-9a-f:]+$') { return $null }
    $idx = $t.IndexOf('::')
    if ($idx -ge 0) {
        if ($t.IndexOf('::', $idx + 2) -ge 0) { return $null }
        $head = Get-V6Part $t.Substring(0, $idx)
        $tail = Get-V6Part $t.Substring($idx + 2)
        if ($null -eq $head -or $null -eq $tail) { return $null }
        $n = $head.Count + $tail.Count
        if ($n -gt 7) { return $null }
        return , @($head + @('0000') * (8 - $n) + $tail)
    }
    $all = Get-V6Part $t
    if ($null -eq $all -or $all.Count -ne 8) { return $null }
    return , $all
}

# Returns loopback, tailscale or other, and sets $script:UrlHost (as written,
# IPv6 in brackets); $null when malformed or ambiguous.
function Get-HttpClass([string] $Url) {
    if (-not ($Url -cmatch '^http://(\[[^\]/]*\]|[^/:\[\]]*)(?::([0-9]*))?(/[^?#@]*)?$')) { return $null }
    $h = $Matches[1]
    $hasPort = $Url -cmatch '^http://(\[[^\]/]*\]|[^/:\[\]]*):'
    $portText = if ($hasPort) { ($Url -replace '^http://(\[[^\]/]*\]|[^/:\[\]]*):([0-9]*).*$', '$2') } else { '' }
    if ($hasPort) {
        if ($portText -notmatch '^[0-9]{1,5}$') { return $null }
        $port = [int] $portText
        if ($port -lt 1 -or $port -gt 65535) { return $null }
    }
    $script:UrlHost = $h
    if ($h.StartsWith('[')) {
        $g = Get-Ipv6Groups $h.Substring(1, $h.Length - 2)
        if ($null -eq $g) { return $null }
        $joined = $g -join ' '
        if ($joined -eq '0000 0000 0000 0000 0000 0000 0000 0000') { return $null }
        if ($joined -eq '0000 0000 0000 0000 0000 0000 0000 0001') {
            if ($h -ceq '[::1]' -and $hasPort) { return 'loopback' }
            return $null
        }
        if ($joined.StartsWith('0000 0000 0000 0000 0000 ffff ')) { return $null }
        if ($joined.StartsWith('fd7a 115c a1e0 ')) { return 'tailscale' }
        return 'other'
    }
    $name = $h; $dotted = $false
    if ($name.EndsWith('.')) { $name = $name.Substring(0, $name.Length - 1); $dotted = $true }
    if ($name -eq '' -or $name.Length -gt 253) { return $null }
    $labels = $name.Split('.')
    foreach ($l in $labels) { if (-not (Test-Label $l)) { return $null } }
    $last = $labels[-1]
    $numeric = ($last -cmatch '^[0-9]+$') -or ($last -match '^0x')
    if ($numeric) {
        if ($dotted -or $labels.Count -ne 4) { return $null }
        foreach ($o in $labels) {
            if ($o -cnotmatch '^(0|[1-9][0-9]{0,2})$' -or [int] $o -gt 255) { return $null }
        }
        if ($labels[0] -eq '127') {
            if ($name -ceq '127.0.0.1' -and $hasPort) { return 'loopback' }
            return $null
        }
        if ($name -eq '0.0.0.0') { return $null }
        if ([int] $labels[0] -eq 100 -and [int] $labels[1] -ge 64 -and [int] $labels[1] -le 127) { return 'tailscale' }
        return 'other'
    }
    $lower = $name.ToLowerInvariant()
    if ($lower -eq 'localhost' -and -not $dotted) {
        if ($h -ceq 'localhost' -and $hasPort) { return 'loopback' }
        return $null
    }
    if ($lower.EndsWith('.ts.net') -and $labels.Count -ge 3) { return 'tailscale' }
    return 'other'
}

# Returns https, loopback, tailscale or other.
function Test-Url([string] $Url) {
    if ($Url -match '[\s?#@"`$\\,]') { Fail '-ServerUrl must be a plain URL without spaces, credentials, query or fragment' }
    if ($Url -cmatch '^https://[^/:]') {
        $u = $null
        if (-not [Uri]::TryCreate($Url, [UriKind]::Absolute, [ref] $u) -or -not $u.Host -or $u.UserInfo) { Fail "invalid -ServerUrl: $Url" }
        return 'https'
    }
    if ($Url -cmatch '^http://') {
        $class = Get-HttpClass $Url
        if ($class) { return $class }
        Fail "invalid -ServerUrl ${Url}: for this computer use exactly http://127.0.0.1:PORT, http://localhost:PORT or http://[::1]:PORT; otherwise host[:port] with a port of 1-65535 and a canonical IPv4 address or DNS name"
    }
    Fail '-ServerUrl must start with https:// or http://'
}

# NO_PROXY for a loopback or Tailscale server: it is always reached directly
# (runner images up to 0.4.0 use proxies from the environment unless
# NO_PROXY matches the URI host as written, IPv6 in brackets). Proxies Docker
# injects are skipped for exactly that host; other destinations keep them.
function Get-DirectArgs($c) {
    if ((Test-Url $c.ServerUrl) -notin 'loopback', 'tailscale') { return @() }
    $h = $script:UrlHost
    $entries = if ($h.StartsWith('[')) { "$h,$($h.Substring(1, $h.Length - 2))" } else { $h }
    return @('--env', "METRO_DIRECT_NO_PROXY=$entries")
}

# url-rule:end

function Get-PlainHttpWarning([string] $Url) {
    $h = ([Uri] $Url).Authority -replace ':[0-9]+$', ''
    "plain HTTP to $h isn't encrypted: anyone on the network between this computer and $h could read the runner token. Use it only on networks you trust, such as Tailscale or a VPN, or use https://."
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
    Test-Url $c.ServerUrl | Out-Null; Test-TokenFile $c.TokenFile; Test-Image $c.Image
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

# Runner images up to 0.4.0 refuse plain http beyond loopback without this
# opt-in; newer ones accept it, ignore it and print their own warning.
function Get-InsecureArgs($c) {
    if ((Test-Url $c.ServerUrl) -in 'tailscale', 'other') { @('--env', 'METRO_ALLOW_INSECURE_HTTP=1') } else { @() }
}

function Get-TokenMount($c) { "type=bind,source=$($c.TokenFile),target=$Secret,readonly" }

function Confirm-Image([string] $Ref) {
    Invoke-Engine image inspect $Ref | Out-Null
    if ($EngineExit -ne 0) {
        Write-Host "Pulling $Ref ..."
        Invoke-Engine pull $Ref | Out-Null
        if ($EngineExit -ne 0) { Fail "cannot pull $Ref. Check the version and your network; Metro runner images are only available once that version is published." }
    }
}

# $Command is one of the fixed texts above; $Arguments reach it as $1, $2, ...
function Invoke-Probe($c, [string] $Command, [string[]] $Arguments = @()) {
    $a = @('run', '--rm', '--pull=never') + (Get-NetworkArgs $c) + @('--read-only', '--cap-drop', 'ALL',
        '--security-opt', 'no-new-privileges:true', '--mount', (Get-TokenMount $c), '--entrypoint', '/bin/bash', $c.Image, '-c', $Command, 'metro-probe') + $Arguments
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
    $script = $RunnerEntry
    $a = @('run', '-d', '--name', $c.Name, '--restart', 'unless-stopped', '--pull=never') + (Get-NetworkArgs $c) + @(
        '--read-only', '--tmpfs', '/tmp:rw,noexec,nosuid,nodev,size=64m',
        '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges:true', '--pids-limit', '256',
        '--mount', "type=volume,source=$($c.Volume),target=/workspace",
        '--mount', (Get-TokenMount $c),
        '--env', "METRO_SERVER_URL=$($c.ServerUrl)") + (Get-InsecureArgs $c) + (Get-DirectArgs $c) + @(
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
            $urlClass = Test-Url $url
            if ($urlClass -eq 'other') { Write-Warning (Get-PlainHttpWarning $url) }
            $tokenPath = (Resolve-Path -LiteralPath $TokenFile).ProviderPath
            Test-TokenFile $tokenPath
            $c = [pscustomobject]@{
                Image = (Get-ImageRef); ServerUrl = $url; TokenFile = $tokenPath
                Name = $(if ($Name) { $Name } else { 'metro-runner' })
                Volume = $(if ($Volume) { $Volume } else { 'metro-runner-workspace' })
                Network = $(if ($urlClass -eq 'loopback') { 'host' } else { 'default' })
            }
            Test-Name 'container name' $c.Name; Test-Name 'volume' $c.Volume
            Assert-Engine
            Confirm-Image $c.Image
            Invoke-Engine volume inspect $c.Volume | Out-Null
            if ($EngineExit -ne 0) { Invoke-Engine volume create $c.Volume | Out-Null; if ($EngineExit -ne 0) { Fail "cannot create volume $($c.Volume)" } }
            if (-not (Invoke-Probe $c $ProbeToken @($Secret))) {
                Fail "the container cannot read $tokenPath. Make sure Docker Desktop can share that folder (Settings > Resources > File sharing)."
            }
            if ($c.Network -eq 'host') {
                $u = [Uri] $url
                $h = if ($u.Host -eq '[::1]') { '::1' } else { '127.0.0.1' }
                if (-not (Invoke-Probe $c $ProbeTcp @($h, [string] $u.Port))) {
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
