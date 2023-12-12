FROM rust:1.74.1

COPY ./ ./

RUN cargo build --release

CMD ["sh", "-c", "./target/release/prefetcharr --jellyfin-url \"${JELLYFIN_URL}\" --sonarr-url \"${SONARR_URL}\" --log-dir \"${LOG_DIR}\""]
