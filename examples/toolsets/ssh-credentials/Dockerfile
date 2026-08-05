FROM alpine:3.21
LABEL md.sycophant.tools='[\
  {"name": "test-cmd", "description": "Run the toolset self-test and return its output. Used by integration tests to verify the toolset execution and result-handling pipeline.", "args": {}}\
]'
COPY dispatch /etc/toolset/dispatch
RUN chmod +x /etc/toolset/dispatch
ARG TARGETARCH
COPY toolset-runtime-linux-${TARGETARCH} /usr/local/bin/toolset-runtime
RUN chmod +x /usr/local/bin/toolset-runtime
ENTRYPOINT ["toolset-runtime"]
