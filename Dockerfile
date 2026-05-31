FROM rust:1.91-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends cmake libclang-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release -p runtime

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/opcua-plc-bridge /usr/local/bin/opcua-plc-bridge
EXPOSE 4840
ENTRYPOINT ["opcua-plc-bridge"]

FROM runtime AS e2e
RUN apt-get update && apt-get install -y --no-install-recommends netcat-openbsd && rm -rf /var/lib/apt/lists/*
