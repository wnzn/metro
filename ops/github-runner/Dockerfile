# One self-hosted GitHub Actions runner for wnzn/metro (label metro-ci-linux-x64).
#
# Same OS and glibc baseline as GitHub's ubuntu-24.04 hosted runners (Ubuntu
# 24.04, glibc 2.39). Every upstream input is pinned: the base image by its OCI
# index digest, the Ubuntu archive by a snapshot timestamp, and the runner and
# NSH archives by SHA-256. Jobs never install packages: everything a job needs
# is in the image, and the job network only reaches GitHub through the egress
# proxy.
#
# Build: docker compose -f ops/github-runner/compose.yaml build

# The bare Ubuntu image has no CA certificates, and apt needs them for the
# HTTPS snapshot archive. Take Debian's ca-certificates bundle from the same
# digest-pinned official Go image the egress proxy builds with. Ubuntu's own
# ca-certificates package replaces it below.
FROM docker.io/library/golang:1.27.3-bookworm@sha256:dc0963f32053e4bc451d24d87e3892d42d01021b61f093e98eb95c0352bb84b0 AS ca

# ubuntu:24.04 OCI index, resolved from Docker Hub on 2026-11-23.
FROM docker.io/library/ubuntu:24.04@sha256:9c7f200c07e3d4a03e7249d2c16bf66a51120fb876aa2242cf31451412543289
# Create the directories first: COPY --chmod would also apply to parents it
# creates, and apt's unprivileged _apt fetcher must be able to traverse them.
RUN install -d -m 0755 /etc/ssl /etc/ssl/certs
COPY --from=ca --chown=root:root --chmod=0644 /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

# Ubuntu archive snapshot (snapshot.ubuntu.com), the only package source, so
# the same build gets the same packages. Move it forward together with the base
# digest. The archive's own GPG signatures (Signed-By) are still verified.
ARG UBUNTU_SNAPSHOT=20261123T000000Z
# actions/runner: update within 63 days of a newer release, or GitHub stops
# assigning jobs (automatic updates are disabled; see entrypoint.sh).
ARG RUNNER_VERSION=2.338.2
ARG RUNNER_SHA256=baebf204307bac6193480852732f0aba2ae97b6c5597c244f4d5558390147c40
# Longbridge Native Shell, the same pin as .github/workflows/ci.yml.
ARG NSH_VERSION=v0.6.1
ARG NSH_SHA256=9a7ac692f1c229df476be8ff83a3c5912d271453b644e313f02cb19d18696070
# The TypeScript 7 compiler NSH v0.6.1 pins for `nsh check`. On first use, nsh
# downloads it from npm (a job can't: the proxy allows only GitHub). Its
# integrity is the one built into nsh 0.6.1 (2f51d86), checked here together
# with the tarball's SHA-256. NSH_TSC below makes nsh use this copy.
ARG TS_PACKAGE=@typescript/typescript-linux-x64
ARG TS_VERSION=7.0.2
ARG TS_INTEGRITY=sha512-EYdf2cNg7rgCWJnxCdJ+F3V39O8ihb37eHAu1LK8oAFizgTQbPOK7zHHXbPt8rX24COqODXeI3sIf0fCXG7H/A==
ARG TS_SHA256=7ecad6f67377e831856367ab062ef394f21506a611405bf8ac0ff039348637d3

SHELL ["/bin/bash", "-euo", "pipefail", "-c"]
ENV DEBIAN_FRONTEND=noninteractive LANG=C.UTF-8 LC_ALL=C.UTF-8

# Runner dependencies (ICU, Kerberos, LTTng, OpenSSL, zlib), job basics, and
# the GTK 4 / WebKitGTK 6 / GStreamer runtime that NSH's Linux host links
# against, plus Xvfb for headless runs.
RUN sed -i -E "s#^URIs: http://(archive|security)\.ubuntu\.com/ubuntu/?\$#URIs: https://snapshot.ubuntu.com/ubuntu/${UBUNTU_SNAPSHOT}/#" \
      /etc/apt/sources.list.d/ubuntu.sources \
 && test "$(grep -h '^URIs:' /etc/apt/sources.list.d/*.sources | sort -u)" = "URIs: https://snapshot.ubuntu.com/ubuntu/${UBUNTU_SNAPSHOT}/" \
 && ! grep -rqs '^deb ' /etc/apt/sources.list /etc/apt/sources.list.d/ \
 && apt_tls=(-o Acquire::https::CAInfo=/etc/ssl/certs/ca-certificates.crt -o Acquire::https::Verify-Peer=true) \
 && apt-get "${apt_tls[@]}" -o APT::Update::Error-Mode=any update \
 && apt-get "${apt_tls[@]}" install -y --no-install-recommends \
      ca-certificates curl git jq python3 tar gzip unzip xz-utils zstd \
      libicu74 libkrb5-3 liblttng-ust1t64 libssl3t64 zlib1g \
      libgtk-4-1 libwebkitgtk-6.0-4 libjavascriptcoregtk-6.0-1 libsoup-3.0-0 \
      libgraphene-1.0-0 libgstreamer1.0-0 libgstreamer-plugins-base1.0-0 \
      gstreamer1.0-plugins-base libpango-1.0-0 libpangocairo-1.0-0 libharfbuzz0b \
      libfontconfig1 libcairo2 fonts-dejavu-core xvfb xauth \
 && update-ca-certificates --fresh >/dev/null \
 && rm -rf /var/lib/apt/lists/*

# Runner and NSH: verified, then root-owned and read-only. Each container start
# copies the runner into its scratch volume, so a job can't change what the
# next start runs.
RUN curl --fail --silent --show-error --location --retry 3 -o /tmp/runner.tgz \
      "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz" \
 && echo "${RUNNER_SHA256}  /tmp/runner.tgz" | sha256sum --check - \
 && mkdir -p /opt/actions-runner && tar -xzf /tmp/runner.tgz -C /opt/actions-runner \
 && curl --fail --silent --show-error --location --retry 3 -o /tmp/nsh.tgz \
      "https://github.com/longbridge/nsh/releases/download/${NSH_VERSION}/nsh-linux-x86_64.tar.gz" \
 && echo "${NSH_SHA256}  /tmp/nsh.tgz" | sha256sum --check - \
 && tar -xzf /tmp/nsh.tgz -C /opt \
 && curl --fail --silent --show-error --location --retry 3 -o /tmp/ts.tgz \
      "https://registry.npmjs.org/${TS_PACKAGE}/-/${TS_PACKAGE#*/}-${TS_VERSION}.tgz" \
 && echo "${TS_SHA256}  /tmp/ts.tgz" | sha256sum --check - \
 && test "$(python3 -I -c 'import base64, hashlib, sys; print("sha512-" + base64.b64encode(hashlib.sha512(open(sys.argv[1], "rb").read()).digest()).decode())' /tmp/ts.tgz)" = "$TS_INTEGRITY" \
 && mkdir /opt/typescript && tar -xzf /tmp/ts.tgz -C /opt/typescript --strip-components=1 --no-same-owner \
 && test -x /opt/typescript/lib/tsc && test -s /opt/typescript/LICENSE && test -s /opt/typescript/NOTICE.txt \
 && rm -f /tmp/runner.tgz /tmp/nsh.tgz /tmp/ts.tgz \
 && chown -R root:root /opt/actions-runner /opt/nsh /opt/typescript \
 && chmod -R go-w /opt/actions-runner /opt/nsh /opt/typescript

# Non-root runner account. /runner is the per-container scratch volume, and
# /state holds only the registration (.runner and credentials).
RUN groupadd --gid 1001 runner \
 && useradd --uid 1001 --gid 1001 --home-dir /home/runner --shell /bin/bash runner \
 && install -d -o runner -g runner -m 0700 /runner /state /home/runner

COPY --chown=root:root --chmod=0755 entrypoint.sh job-started.sh job-completed.sh /opt/metro-runner/

ENV PATH=/opt/nsh/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME=/home/runner \
    NSH_TSC=/opt/typescript/lib/tsc
USER 1001:1001
# The compiler works with no network and an empty HOME.
RUN --network=none test -z "$(ls -A /home/runner)" && "$NSH_TSC" --version && nsh --version
WORKDIR /home/runner
ENTRYPOINT ["/opt/metro-runner/entrypoint.sh"]
CMD ["run"]
