FROM rust:1-bookworm AS build

WORKDIR /app
RUN apt-get update \
  && apt-get install -y --no-install-recommends pkg-config libsqlite3-dev ca-certificates protobuf-compiler libprotobuf-dev \
  && rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo build --release -p local_multi_agent_service --bin warp-local-multi-agent

FROM debian:bookworm-slim

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates curl \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --create-home --uid 10001 warp \
  && mkdir -p /data \
  && chown -R warp:warp /data

COPY --from=build /app/target/release/warp-local-multi-agent /usr/local/bin/warp-local-multi-agent

ENV HOST=0.0.0.0 \
    PORT=8787 \
    LOCAL_GRAPHQL_DB_PATH=/data/local-graphql.sqlite \
    LOCAL_SERVICE_LOG_PATH=/data/local-service.log

USER warp
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
  CMD curl -fsS "http://127.0.0.1:${PORT}/health" >/dev/null || exit 1

CMD ["warp-local-multi-agent"]
