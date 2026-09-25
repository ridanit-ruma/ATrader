# syntax=docker/dockerfile:1
# Dashboard, then the binary that embeds it, then a slim runtime image.

FROM node:24-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web ./
RUN npm run build

FROM rust:1.96-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock holidays.toml fees.toml ./
COPY migrations ./migrations
COPY src ./src
COPY --from=web /web/dist ./web/dist
RUN cargo build --release --locked

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Match the host user that owns the secret files (compose mounts them with host permissions).
ARG UID=1000
RUN useradd --system --uid ${UID} --home-dir /var/lib/atrader atrader \
    && mkdir -p /var/lib/atrader && chown atrader /var/lib/atrader
COPY --from=build /src/target/release/atrader /usr/local/bin/atrader
USER atrader
ENV ATRADER_STATE_DIR=/var/lib/atrader
ENTRYPOINT ["atrader"]
CMD ["serve"]
