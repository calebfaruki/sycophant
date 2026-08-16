# The prompt toolset: a published image the controller launches as a Job like
# any other toolset, built FROM the toolset base image.
#
# The entrypoint override is forced, not chosen: the prompt binary drives
# GetTurn/AwaitTurnCancel/StreamTurnResult while `toolset-runtime` drives
# GetToolCall, and turn and tool call keep separate verb sets.
ARG BASE_IMAGE=toolset-base:local
FROM ${BASE_IMAGE}
ARG TARGETARCH
COPY prompt-toolset-linux-${TARGETARCH} /usr/local/bin/prompt-toolset
RUN chmod +x /usr/local/bin/prompt-toolset
ENTRYPOINT ["prompt-toolset"]
