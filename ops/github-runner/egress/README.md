# GitHub runner egress

Build with this directory as the Docker context. The image runs as
`65532:65532`, needs no writable filesystem, and serves HTTP/1 CONNECT on
port 3128. It does not terminate TLS, read tunneled HTTP, or log requests.

## Compose contract

- The runner joins only its dedicated, `internal: true` bridge with the
  proxy. Give the proxy the service name `egress` on that bridge and set
  the runner's HTTPS proxy to `http://egress:3128`. Only the proxy also joins
  a separate outbound bridge; do not attach unrelated services to either.
- Use `read_only: true`, `cap_drop: [ALL]`, `security_opt:
  [no-new-privileges:true]`, and a small `/tmp` tmpfs (`noexec,nosuid,nodev`).
  No published ports, host binds, Docker socket, or credentials.
- The image already defines the health check. A Compose override can use
  `["CMD", "/usr/local/bin/egress", "-healthcheck"]` with a three-second timeout.
  It checks a denied request over loopback with no DNS or upstream dependency.

## Scope

The allowlist follows [GitHub's self-hosted runner communication reference](https://docs.github.com/en/actions/reference/runners/self-hosted-runners#accessible-domains-by-function):
`github.com`, `api.github.com`, and `*.actions.githubusercontent.com` for
registration and the Actions service; `codeload.github.com` for action
downloads and checkout archives. The Actions suffix includes the results
receiver. Azure Blob Storage is **not** enabled: the official storage
suffix covers unrelated Azure tenants. Features that require direct blob
access (some logs, summaries, artifacts, and caches), built-in runner
updates, LFS, registries, and external workflow dependencies are outside
this baseline. Update the runner by rebuilding its image.

The proxy checks the CONNECT authority before DNS, rejects any answer set
containing a non-public or special-use address, and dials only a validated
numeric address on port 443. This also applies to addresses reached through
CNAMEs; CNAMEs don't extend the hostname allowlist. The address policy is
based on the IANA special-purpose registries for
[IPv4](https://www.iana.org/assignments/iana-ipv4-special-registry/iana-ipv4-special-registry.xhtml)
and [IPv6](https://www.iana.org/assignments/iana-ipv6-special-registry/iana-ipv6-special-registry.xhtml)
and also blocks the Azure host virtual IP. This is a destination boundary,
not a repository- or API-permission boundary within GitHub. It relies on the
deployment isolating the runner from all other network paths and on the
proxy using the deployment's trusted DNS and outbound routing.

`go test ./...` uses local socket fixtures and stubbed DNS and needs no
access to GitHub or the Internet.
