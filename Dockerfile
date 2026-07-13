# ---- build ----
FROM rust:1-bookworm AS build
RUN rustup target add wasm32-unknown-unknown \
 && cargo install --locked trunk wasm-pack
WORKDIR /app
COPY . .
# Server binary + frontend WASM
RUN cargo build --release -p sheshbesh-server
WORKDIR /app/frontend
RUN ./build-worker.sh && trunk build --release

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/sheshbesh-server /usr/local/bin/
COPY --from=build /app/frontend/dist /app/static
ENV STATIC_DIR=/app/static PORT=8080
EXPOSE 8080
CMD ["sheshbesh-server"]
