# Kora — PostgreSQL image (default): static musl binary, multi-arch via xx.
#   docker build -f dockerfiles/postgres.Dockerfile -t kora .      # local, native arch
#   just build                                                     # multi-arch + push
# The Oracle image lives in dockerfiles/oracle.Dockerfile (glibc + Instant Client).
# The chart/CLI are identical across both; only the backend driver + base differ.

# -- Cross-compilation helper (static musl via xx) --
FROM --platform=$BUILDPLATFORM tonistiigi/xx AS xx

# -- Builder: static musl binary via xx-cargo --
FROM --platform=$BUILDPLATFORM rust:1.96-alpine AS builder
COPY --from=xx / /
RUN apk add clang cmake lld
RUN rustup target add $(xx-cargo --print-target-triple)

WORKDIR /usr/src
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY migrations/ migrations/

# NOTE: do NOT build with --features oracle here — the `oracle` crate is a thick
# ODPI-C driver needing glibc + Oracle Instant Client at runtime, so it cannot be
# a static-musl binary. Build the Oracle image from dockerfiles/oracle.Dockerfile.
ARG TARGETPLATFORM
RUN xx-apk add --no-cache musl-dev zlib-dev zlib-static gcc
RUN xx-cargo build --release --bin kora
RUN xx-verify --static ./target/$(xx-cargo --print-target-triple)/release/kora

RUN mkdir -p /image && \
    cp target/$(xx-cargo --print-target-triple)/release/kora /image/kora

# -- Runtime: Alpine + tini --
FROM alpine:3.23
LABEL org.opencontainers.image.source="https://github.com/Popsink/Kora" \
      org.opencontainers.image.description="Kora — Confluent-compatible Schema Registry" \
      org.opencontainers.image.licenses="MIT"
RUN apk add --no-cache tini
COPY --from=builder /image/kora /usr/local/bin/kora
COPY migrations/ /app/migrations/
WORKDIR /app
ENV HOST=0.0.0.0 PORT=8080
EXPOSE 8080
USER 65534
HEALTHCHECK --interval=5s --timeout=3s --start-period=10s --retries=3 \
    CMD wget -qO- http://localhost:8080/health || exit 1
ENTRYPOINT ["/sbin/tini", "--"]
CMD ["/usr/local/bin/kora"]
