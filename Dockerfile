FROM rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS rust-base

FROM rust-base AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
RUN cargo build --locked --release --bin metro-server --bin metro-runner

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc AS runtime
COPY --from=rust-base /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
# Pin the signed Debian archives as well as the base image. Refresh together.
RUN rm -f /etc/apt/sources.list.d/debian.sources \
    && printf '%s\n' \
      'deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/20261120T000000Z/ bookworm main' \
      'deb [check-valid-until=no] https://snapshot.debian.org/archive/debian-security/20261120T000000Z/ bookworm-security main' \
      > /etc/apt/sources.list \
    && apt-get -o APT::Update::Error-Mode=any update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

FROM runtime AS server
RUN groupadd --gid 10001 metro \
    && useradd --uid 10001 --gid 10001 --no-create-home --no-log-init --shell /usr/sbin/nologin metro \
    && install -d -o metro -g metro /data
COPY --from=build /build/target/release/metro-server /usr/local/bin/metro-server
USER 10001:10001
WORKDIR /data
ENV METRO_BIND=0.0.0.0:8787 \
    METRO_DB=/data/metro.sqlite3
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/metro-server"]

FROM runtime AS runner
ARG RUNNER_UID=1000
ARG RUNNER_GID=1000
RUN groupadd --gid "$RUNNER_GID" metro \
    && useradd --uid "$RUNNER_UID" --gid "$RUNNER_GID" --create-home --no-log-init --shell /usr/sbin/nologin metro \
    && install -d -o metro -g metro /workspace
COPY --from=build /build/target/release/metro-runner /usr/local/bin/metro-runner
USER ${RUNNER_UID}:${RUNNER_GID}
WORKDIR /workspace
ENV HOME=/home/metro \
    METRO_WORKSPACE=/workspace \
    METRO_SANDBOX=none
ENTRYPOINT ["/usr/local/bin/metro-runner"]
