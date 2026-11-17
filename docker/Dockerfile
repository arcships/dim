ARG BASE_IMAGE=debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
FROM ${BASE_IMAGE}

ARG TARGETARCH
ARG VERSION=0.0.0
ARG SOURCE_SHA=unknown

LABEL org.opencontainers.image.title="Dim" \
      org.opencontainers.image.description="Dim coding agent CLI and App Server" \
      org.opencontainers.image.source="https://github.com/arcships/dim" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${SOURCE_SHA}" \
      org.opencontainers.image.licenses="MIT"

RUN groupadd --gid 1000 dim \
 && useradd --uid 1000 --gid 1000 --create-home --home-dir /home/dim --shell /bin/bash dim \
 && apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git tini \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /workspace \
 && chown dim:dim /workspace /home/dim \
 && ln -s /opt/dim/bin/dimcode /usr/local/bin/dimcode \
 && ln -s /opt/dim/bin/dimcode /usr/local/bin/dim

COPY rootfs-${TARGETARCH}/ /

ENV HOME=/home/dim \
    LANG=C.UTF-8

USER dim
WORKDIR /workspace
ENTRYPOINT ["/usr/bin/tini","--","/opt/dim/bin/dimcode"]
