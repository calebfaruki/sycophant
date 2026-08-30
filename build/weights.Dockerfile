# The weights image: the GGUF as one standard OCI layer, plus a static busybox
# so the copy-delivery init container has an exec to run. scratch has no shell,
# so that init container copies with /busybox cp.
FROM busybox:musl AS shell
FROM scratch
ARG GGUF=model.gguf
ARG WEIGHTS_PATH=/weights/model.gguf
COPY --from=shell /bin/busybox /busybox
COPY ${GGUF} ${WEIGHTS_PATH}
