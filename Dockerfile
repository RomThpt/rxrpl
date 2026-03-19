FROM rust:1.85-bookworm AS builder
WORKDIR /build
COPY . .
RUN apt-get update && apt-get install -y libclang-dev cmake && rm -rf /var/lib/apt/lists/*
RUN cargo build --release --bin rxrpl

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/rxrpl /usr/local/bin/rxrpl
EXPOSE 5005 51235
ENTRYPOINT ["rxrpl"]
