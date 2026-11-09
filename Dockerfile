# Built by .github/workflows/release.yml using prebuilt linux binaries.
# Expects dist/${TARGETARCH}/trivia to exist (amd64 and arm64).
# Validate locally with `just docker-test`.

# distroless has no shell, so create the nonroot-owned data dir here.
FROM busybox:1.37 AS data
RUN mkdir /data

FROM gcr.io/distroless/cc-debian13:nonroot

ARG TARGETARCH

COPY --from=data --chown=nonroot:nonroot /data /data
COPY --chmod=755 dist/${TARGETARCH}/trivia /usr/local/bin/trivia

ENV BIND_ADDR=0.0.0.0:3000 \
    TRIVIA_DB=/data/trivia.db

VOLUME ["/data"]
EXPOSE 3000

ENTRYPOINT ["/usr/local/bin/trivia"]
CMD ["www"]
