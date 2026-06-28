FROM rust:1.88-slim-bookworm AS builder
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release -p youtubeopen --bin youtubeopen

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y \
    ca-certificates openssl ffmpeg yt-dlp \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/youtubeopen /usr/local/bin/
COPY --from=builder /app/frontend-dist /app/frontend-dist
EXPOSE 8080
ENV PORT=8080
CMD ["youtubeopen"]