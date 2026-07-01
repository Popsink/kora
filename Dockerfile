# -- Cross-compilation helper --
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

ARG TARGETPLATFORM
RUN xx-apk add --no-cache musl-dev zlib-dev zlib-static gcc
RUN xx-cargo build --release --bin kora
RUN xx-verify --static ./target/$(xx-cargo --print-target-triple)/release/kora

RUN mkdir -p /image && \
    cp target/$(xx-cargo --print-target-triple)/release/kora /image/kora

# -- Runtime: Alpine + tini --
FROM alpine:3.24

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
