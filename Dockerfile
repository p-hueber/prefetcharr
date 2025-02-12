FROM rust:alpine as builder

RUN apk add libc-dev

WORKDIR /app
ADD . /app
RUN cargo build --release


FROM alpine:latest

ENV INTERVAL=900
ENV REMAINING_EPISODES=2

COPY --from=builder /app/target/release/prefetcharr /

CMD ["sh", "-c", "./prefetcharr \
  --media-server-type \"${MEDIA_SERVER_TYPE}\" \
  --media-server-url \"${MEDIA_SERVER_URL}\" \
  --sonarr-url \"${SONARR_URL}\" \
  --log-dir \"${LOG_DIR}\" \
  --interval \"${INTERVAL}\" \
  --remaining-episodes \"${REMAINING_EPISODES}\" \
  --users \"${USERS:---users}\" \
  --libraries \"${LIBRARIES:---libraries}\" \
  --connection-retries 6 \
  "]
