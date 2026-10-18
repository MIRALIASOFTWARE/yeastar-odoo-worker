# ---------- Etapa 1: compilar ----------
FROM docker.uclv.cu/rust:1-bookworm AS builder
WORKDIR /app

# Capa de dependencias (se reutiliza mientras Cargo.toml no cambie)
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

# Código real
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ---------- Etapa 2: imagen final ----------
FROM docker.uclv.cu/debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --no-create-home --shell /usr/sbin/nologin app

COPY --from=builder /app/target/release/yeastar-odoo-gateway /usr/local/bin/yeastar-odoo-gateway

USER app
ENV BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
CMD ["yeastar-odoo-gateway"]
