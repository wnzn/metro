FROM docker.io/library/golang:1.27.3-bookworm@sha256:dc0963f32053e4bc451d24d87e3892d42d01021b61f093e98eb95c0352bb84b0 AS build
WORKDIR /src
COPY go.mod main.go main_test.go ./
RUN --network=none GOTOOLCHAIN=local CGO_ENABLED=0 go test ./... && \
    GOTOOLCHAIN=local CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /egress .

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
COPY --from=build /egress /usr/local/bin/egress
USER 65532:65532
EXPOSE 3128
HEALTHCHECK --interval=15s --timeout=3s --start-period=5s CMD ["/usr/local/bin/egress", "-healthcheck"]
ENTRYPOINT ["/usr/local/bin/egress"]
