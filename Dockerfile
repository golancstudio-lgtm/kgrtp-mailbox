# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY templates ./templates
COPY static ./static
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /app --create-home appuser
WORKDIR /app
COPY --from=builder /src/target/release/kgrtp-mailbox /app/kgrtp-mailbox
USER 10001
ENV PORT=8080
EXPOSE 8080
ENTRYPOINT ["/app/kgrtp-mailbox"]
