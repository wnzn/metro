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

# ubuntu:24.04 OCI index, resolved from Docker Hub on 2026-11-23.
FROM ubuntu:24.04@sha256:9c7f200c07e3d4a03e7249d2c16bf66a51120fb876aa2242cf31451412543289

# Ubuntu archive snapshot (snapshot.ubuntu.com), so the same build gets the
# same packages. Move it forward together with the base digest.
ARG UBUNTU_SNAPSHOT=20261123T000000Z
# actions/runner: update within 63 days of a newer release, or GitHub stops
# assigning jobs (automatic updates are disabled; see entrypoint.sh).
ARG RUNNER_VERSION=2.338.2
ARG RUNNER_SHA256=baebf204307bac6193480852732f0aba2ae97b6c5597c244f4d5558390147c40
# Longbridge Native Shell, the same pin as .github/workflows/ci.yml.
ARG NSH_VERSION=v0.6.1
ARG NSH_SHA256=9a7ac692f1c229df476be8ff83a3c5912d271453b644e313f02cb19d18696070

SHELL ["/bin/bash", "-euo", "pipefail", "-c"]
ENV DEBIAN_FRONTEND=noninteractive LANG=C.UTF-8 LC_ALL=C.UTF-8

# Runner dependencies (ICU, Kerberos, LTTng, OpenSSL, zlib), job basics, and
# the GTK 4 / WebKitGTK 6 / GStreamer runtime that NSH's Linux host links
# against, plus Xvfb for headless runs.
RUN apt-get update --snapshot "$UBUNTU_SNAPSHOT" \
 && apt-get install -y --no-install-recommends --snapshot "$UBUNTU_SNAPSHOT" \
      ca-certificates curl git jq python3 tar gzip unzip xz-utils zstd \
      libicu74 libkrb5-3 liblttng-ust1t64 libssl3t64 zlib1g \
      libgtk-4-1 libwebkitgtk-6.0-4 libjavascriptcoregtk-6.0-1 libsoup-3.0-0 \
      libgraphene-1.0-0 libgstreamer1.0-0 libgstreamer-plugins-base1.0-0 \
      gstreamer1.0-plugins-base libpango-1.0-0 libpangocairo-1.0-0 libharfbuzz0b \
      libfontconfig1 libcairo2 fonts-dejavu-core xvfb xauth \
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
 && rm -f /tmp/runner.tgz /tmp/nsh.tgz \
 && chown -R root:root /opt/actions-runner /opt/nsh \
 && chmod -R go-w /opt/actions-runner /opt/nsh

# Non-root runner account. /runner is the per-container scratch volume, and
# /state holds only the registration (.runner and credentials).
RUN groupadd --gid 1001 runner \
 && useradd --uid 1001 --gid 1001 --home-dir /home/runner --shell /bin/bash runner \
 && install -d -o runner -g runner -m 0700 /runner /state /home/runner

COPY --chown=root:root --chmod=0755 entrypoint.sh job-started.sh job-completed.sh /opt/metro-runner/

ENV PATH=/opt/nsh/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME=/home/runner
USER 1001:1001
WORKDIR /home/runner
ENTRYPOINT ["/opt/metro-runner/entrypoint.sh"]
CMD ["run"]
