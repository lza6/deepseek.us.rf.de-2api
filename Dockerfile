# ===== 构建阶段 =====
FROM rust:1.95-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
# 预热依赖缓存
RUN mkdir src && echo 'fn main(){}' > src/main.rs && echo '' > src/lib.rs \
    && cargo build --release 2>/dev/null || true
COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release

# ===== 运行阶段 =====
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/target/release/deepseek-es-2api /usr/local/bin/deepseek-es-2api
COPY config.example.json /app/config.example.json
ENV LISTEN_ADDR=0.0.0.0:47833
EXPOSE 47833
ENTRYPOINT ["deepseek-es-2api"]
CMD ["--config", "/app/config.json"]
