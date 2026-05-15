FROM alpine:3.21
LABEL md.sycophant.tools='[\
  {"name": "ssh-exec", "description": "Run a remote command over SSH on a host the chamber has been credentialed for.", "args": {\
    "host":    {"type": "string", "required": true, "env": "HOST",    "description": "SSH target as user@host or host."},\
    "command": {"type": "string", "required": true, "env": "COMMAND", "description": "Remote command to execute."}\
  }}\
]'
RUN apk add --no-cache openssh-client
COPY dispatch /etc/chamber/dispatch
RUN chmod +x /etc/chamber/dispatch
ARG TARGETARCH
COPY airlock-runtime-linux-${TARGETARCH} /usr/local/bin/airlock-runtime
RUN chmod +x /usr/local/bin/airlock-runtime
ENTRYPOINT ["airlock-runtime"]
