FROM rust:1.92-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/resend-email-forwarding /usr/local/bin/
EXPOSE 3000
CMD ["resend-email-forwarding"]
