# The harness image: the harness binary plus static GNU gzip and logrotate,
# which compact (logrotate) and ReadArchive (gzip) run by fork and exec. No
# shell, no BusyBox.
#
# logrotate 3.22.0 (GitHub release, signed tag 41efb71) and GNU gzip 1.14
# (from the 2025-04-09 release mail). logrotate is GPL-2.0-or-later and gzip
# is GPL-3.0-or-later; their source is the exact tarballs pinned below.
FROM alpine:3.21@sha256:87a4361f6c819618a6bafa2ed5228714cb0bb22195d2b49c95eb26799990128a AS tools
RUN apk add --no-cache build-base popt-dev popt-static xz wget
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

FROM scratch
ARG TARGETARCH
# /bin/gzip is where logrotate's compiled-in compress command looks.
COPY --from=tools /src/logrotate-3.22.0/logrotate /bin/logrotate
COPY --from=tools /src/gzip-1.14/gzip /bin/gzip
COPY --chmod=755 harness-linux-musl-${TARGETARCH} /app
ENTRYPOINT ["/app"]
