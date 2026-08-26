# Built by .github/workflows/release.yml using prebuilt linux binaries.
# Expects dist/${TARGETARCH}/trivia to exist (amd64 and arm64).
# Those binaries are compiled with `--features s3vectors`, so the S3 Vectors
# backend is available at runtime via `backend = "s3vectors"` / TRIVIA_BACKEND.
FROM debian:trixie-slim

ARG TARGETARCH

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libgomp1 \
 && rm -rf /var/lib/apt/lists/*

COPY dist/${TARGETARCH}/trivia /usr/local/bin/trivia
RUN chmod +x /usr/local/bin/trivia

ENV BIND_ADDR=0.0.0.0:3000 \
    TRIVIA_DB=/data/trivia.db

VOLUME ["/data"]
EXPOSE 3000

ENTRYPOINT ["/usr/local/bin/trivia"]
CMD ["www"]
