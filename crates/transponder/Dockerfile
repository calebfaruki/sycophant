FROM rust:1.85 AS builder

RUN apt-get update && apt-get install -y protobuf-compiler musl-tools && \
    rustup target add x86_64-unknown-linux-musl

WORKDIR /build
COPY . .
RUN cargo build --release --target x86_64-unknown-linux-musl -p transponder

FROM scratch
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/transponder /usr/local/bin/transponder
ENTRYPOINT ["transponder"]
