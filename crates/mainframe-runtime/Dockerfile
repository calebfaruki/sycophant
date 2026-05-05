FROM rust:1.85 AS builder

RUN apt-get update && apt-get install -y protobuf-compiler musl-tools && \
    rustup target add x86_64-unknown-linux-musl

WORKDIR /build
COPY . .
RUN cargo build --release --target x86_64-unknown-linux-musl -p mainframe-runtime

FROM alpine:3.21
RUN apk add --no-cache git
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/mainframe-runtime /usr/local/bin/mainframe-runtime
ENTRYPOINT ["mainframe-runtime"]
