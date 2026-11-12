# The harness image: the harness binary plus static GNU gzip, logrotate and
# rclone. Compact (logrotate) and ReadArchive (gzip) call them by fork and
# exec. The instructions-sync init container calls /bin/rclone directly. No
# shell, no BusyBox.
#
# logrotate 3.22.0 (GitHub release, signed tag 41efb71) and GNU gzip 1.14
# (from the 2025-04-09 release mail). logrotate is GPL-2.0-or-later and gzip
# is GPL-3.0-or-later; their source is the exact tarballs pinned below.
# rclone 1.75.1 is the upstream static release binary, pinned by the signed
# SHA256SUMS of that release. rclone is MIT.
FROM alpine:3.21@sha256:87a4361f6c819618a6bafa2ed5228714cb0bb22195d2b49c95eb26799990128a AS tools
ARG TARGETARCH
RUN apk add --no-cache build-base popt-dev popt-static xz wget unzip
WORKDIR /src
RUN wget -q https://github.com/logrotate/logrotate/releases/download/3.22.0/logrotate-3.22.0.tar.xz \
 && echo "42b4080ee99c9fb6a7d12d8e787637d057a635194e25971997eebbe8d5e57618  logrotate-3.22.0.tar.xz" | sha256sum -c - \
 && tar xf logrotate-3.22.0.tar.xz
RUN wget -q https://ftp.gnu.org/gnu/gzip/gzip-1.14.tar.xz \
 && echo "01a7b881bd220bfdf615f97b8718f80bdfd3f6add385b993dcf6efd14e8c0ac6  gzip-1.14.tar.xz" | sha256sum -c - \
 && tar xf gzip-1.14.tar.xz
RUN cd logrotate-3.22.0 && ./configure --without-acl --without-selinux LDFLAGS=-static \
 && make && strip logrotate
RUN cd gzip-1.14 && ./configure LDFLAGS=-static && make && strip gzip
RUN case "$TARGETARCH" in \
      amd64) sum=982b5aa772841168f8e380f139e9e787b2a105403e32b94da8676a0e1c0a13ab ;; \
      arm64) sum=03f2504174034b6d004152ed7369251c9a9ec1f7e0836eda420f5c7a5ec0dff9 ;; \
      *) echo "no rclone pin for $TARGETARCH" >&2; exit 1 ;; \
    esac \
 && wget -q https://downloads.rclone.org/v1.75.1/rclone-v1.75.1-linux-${TARGETARCH}.zip \
 && echo "$sum  rclone-v1.75.1-linux-${TARGETARCH}.zip" | sha256sum -c - \
 && unzip -q rclone-v1.75.1-linux-${TARGETARCH}.zip

FROM scratch
ARG TARGETARCH
# /bin/gzip is where logrotate's compiled-in compress command looks.
COPY --from=tools /src/logrotate-3.22.0/logrotate /bin/logrotate
COPY --from=tools /src/gzip-1.14/gzip /bin/gzip
COPY --from=tools /src/rclone-v1.75.1-linux-${TARGETARCH}/rclone /bin/rclone
COPY --chmod=755 harness-linux-musl-${TARGETARCH} /app
ENTRYPOINT ["/app"]
