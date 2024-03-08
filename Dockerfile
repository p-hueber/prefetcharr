FROM rust:1.75.0

COPY ./ ./

RUN cargo build --release

CMD ["bash", "-c", "./target/release/prefetcharr \
  --media-server-type \"${MEDIA_SERVER_TYPE}\" \
  --media-server-url \"${MEDIA_SERVER_URL}\" \
  --sonarr-url \"${SONARR_URL}\" \
  --log-dir \"${LOG_DIR}\" \
  --interval \"${INTERVAL:-900}\" \
  --remaining-episodes \"${REMAINING_EPISODES:-2}\" \
  "]
