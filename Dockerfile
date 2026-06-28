# Сборка фронтенда (Leptos + WASM) через Trunk и раздача статики через nginx.
# Движок `sheshbesh` собирается из корня воркспейса (frontend зависит от него по пути).

# ---- build ----
FROM rust:1-bookworm AS build
RUN rustup target add wasm32-unknown-unknown \
 && cargo install --locked trunk wasm-pack
WORKDIR /app
COPY . .
WORKDIR /app/frontend
RUN ./build-worker.sh && trunk build --release
# Готовая статика — в /app/frontend/dist

# ---- serve ----
FROM nginx:alpine
COPY --from=build /app/frontend/dist /usr/share/nginx/html
COPY deploy/nginx.conf /etc/nginx/conf.d/default.conf
EXPOSE 8080
