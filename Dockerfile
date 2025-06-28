FROM rust:alpine as builder

RUN apk add libc-dev

WORKDIR /app
ADD . /app
RUN cargo build --release


FROM alpine:latest

COPY --from=builder /app/target/release/prefetcharr /
COPY --from=builder /app/docker/cmd.sh /
COPY --from=builder /app/ATTRIBUTION.md /

CMD ["/cmd.sh"]
