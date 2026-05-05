### Build stage
FROM rust:1.85-bookworm AS builder
WORKDIR /build

# System deps for rocksdb / native crates
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libclang-dev \
        cmake \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Cache deps separately when possible (full source copy keeps it simple)
COPY . .
RUN cargo build --release --bin rxrpl

### Runtime stage
FROM debian:bookworm-slim AS runtime

ARG RXRPL_UID=10001
ARG RXRPL_GID=10001

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid ${RXRPL_GID} rxrpl \
    && useradd  --system --uid ${RXRPL_UID} --gid ${RXRPL_GID} \
                --home-dir /var/lib/rxrpl --shell /usr/sbin/nologin rxrpl \
    && mkdir -p /var/lib/rxrpl /etc/rxrpl \
    && chown -R rxrpl:rxrpl /var/lib/rxrpl /etc/rxrpl

COPY --from=builder /build/target/release/rxrpl /usr/local/bin/rxrpl
COPY config/rxrpl-mainnet.toml    /etc/rxrpl/rxrpl-mainnet.toml
COPY config/rxrpl-testnet.toml    /etc/rxrpl/rxrpl-testnet.toml
COPY config/rxrpl-standalone.toml /etc/rxrpl/rxrpl-standalone.toml

USER rxrpl:rxrpl
WORKDIR /var/lib/rxrpl

# RPC + P2P
EXPOSE 5005 51235

# Liveness: server_info via JSON-RPC. Override RXRPL_RPC_URL if RPC bind differs.
ENV RXRPL_RPC_URL=http://127.0.0.1:5005
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD curl -fsS -H 'content-type: application/json' \
        --data '{"method":"server_info","params":[{}]}' \
        "$RXRPL_RPC_URL" >/dev/null || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/rxrpl"]
CMD ["run", "--config", "/etc/rxrpl/rxrpl-mainnet.toml", "--data-dir", "/var/lib/rxrpl"]
