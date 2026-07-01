# Kora — Oracle image (opt-in): the `oracle` crate is a thick ODPI-C driver, so it
# needs glibc + the Oracle Instant Client at runtime. This is a dynamically-linked
# glibc binary on an Oracle Linux base with Instant Client bundled (NOT the static
# musl image dockerfiles/postgres.Dockerfile ships; ~10x larger).
#   docker build -f dockerfiles/oracle.Dockerfile -t kora:oracle .   # local, native arch
#   just build-oracle                                                # multi-arch + push

# -- Oracle builder: glibc + gcc/make to compile the bundled ODPI-C --
FROM rust:1.96-bookworm AS oracle-builder
RUN apt-get update && apt-get install -y --no-install-recommends build-essential && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /usr/src
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY migrations/ migrations/
# Default target on rust:1.96-bookworm is the glibc triple of the build arch.
RUN cargo build --release --features oracle --bin kora && \
    mkdir -p /image && cp target/release/kora /image/kora

# -- Oracle runtime: Oracle Linux 9 slim + Instant Client (basiclite) --
# OL9 (glibc 2.34) matches the glibc the bookworm builder links against; OL8
# (glibc 2.28) is too old and the binary won't load there.
FROM oraclelinux:9-slim
LABEL org.opencontainers.image.source="https://github.com/Popsink/Kora" \
      org.opencontainers.image.description="Kora — Confluent-compatible Schema Registry (Oracle)" \
      org.opencontainers.image.licenses="MIT"
# `oracle-instantclient-release-23ai-el9` enables Oracle's Instant Client yum repo
# (the plain `-el9` name ships only as a non-installable .src); basiclite then
# resolves the arch-appropriate RPM (amd64 + aarch64 both on Oracle's yum) and
# drops a config in /etc/ld.so.conf.d + runs ldconfig, so no LD_LIBRARY_PATH is
# needed. tini/curl come from EPEL (`oracle-epel-release-el9`).
RUN microdnf install -y oracle-instantclient-release-23ai-el9 oracle-epel-release-el9 && \
    microdnf install -y oracle-instantclient-basiclite tini curl && \
    microdnf clean all
COPY --from=oracle-builder /image/kora /usr/local/bin/kora
COPY migrations/ /app/migrations/
WORKDIR /app
ENV HOST=0.0.0.0 PORT=8080
EXPOSE 8080
USER 65534
HEALTHCHECK --interval=5s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -fsS http://localhost:8080/health || exit 1
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/kora"]
