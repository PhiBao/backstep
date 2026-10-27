# ---- builder ----
FROM rust:1.90-slim-bookworm AS builder
RUN apt-get update && apt-get install -y pkg-config build-essential && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && cargo build --release || true
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ffmpeg ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/backstep ./backstep
ENV PORT=8787 BACKSTEP_DATA_DIR=/data
EXPOSE 8787
CMD ["./backstep"]
