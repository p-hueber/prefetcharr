FROM rust:1.75.0

COPY ./ ./

RUN cargo build --release

CMD ["sh", "-c", "./target/release/prefetcharr \
  --jellyfin-url \"${JELLYFIN_URL}\" \
  --sonarr-url \"${SONARR_URL}\" \
  --log-dir \"${LOG_DIR}\" \
  --interval \"${INTERVAL}\" \
  --remaining-episodes \"${REMAINING_EPISODES}\" \
  "]
