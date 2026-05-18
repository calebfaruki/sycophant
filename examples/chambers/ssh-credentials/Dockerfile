FROM alpine:3.21
LABEL md.sycophant.tools='[\
  {"name": "test-cmd", "description": "Run the chamber self-test and return its output. Used by integration tests to verify the chamber execution and result-handling pipeline.", "args": {}}\
]'
COPY dispatch /etc/chamber/dispatch
RUN chmod +x /etc/chamber/dispatch
ARG TARGETARCH
COPY airlock-runtime-linux-${TARGETARCH} /usr/local/bin/airlock-runtime
RUN chmod +x /usr/local/bin/airlock-runtime
ENTRYPOINT ["airlock-runtime"]
