# The Flux remote execution system, as an OCI image.
#
# The image runs one thing: `flux system serve`. It carries no bearer token, no TLS private key and
# no workspace content — all three are mounted by the operator at run time, which is what lets the
# same image bytes serve every deployment. See deploy/README.md for the build contract and
# website/docs/remote-system-deployment.md for the operator-facing profile.
#
# Build through deploy/container/build-image.sh, which stages the binary and stamps the release
# version. The build context is that staging directory, not the repository: the only thing this
# Dockerfile copies is a single Linux `flux` binary.

# A glibc base whose runtime is newer than the release builder's. The published
# flux-cli-x86_64-unknown-linux-gnu archive is built on an older glibc than this base provides, so
# it runs here unmodified; a statically linked binary runs here too.
ARG BASE_IMAGE=debian:trixie-slim
FROM ${BASE_IMAGE}

# Stamped by build-image.sh from [workspace.package].version, so an image is traceable to the
# release whose binary it carries.
ARG FLUX_VERSION=0.0.0-unreleased

LABEL org.opencontainers.image.title="flux-system" \
      org.opencontainers.image.description="Flux remote execution system — flux system serve" \
      org.opencontainers.image.version="${FLUX_VERSION}" \
      org.opencontainers.image.source="https://github.com/codewandler/flux" \
      org.opencontainers.image.documentation="https://github.com/codewandler/flux/blob/main/website/docs/remote-system-deployment.md" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"

# ca-certificates: the daemon's own listener uses the mounted certificate, but guarded outbound
# network effects executed on this substrate need a trust store.
# The runtime identity is a fixed non-root uid/gid so a PodSecurity `runAsNonRoot` admission check
# and a `--read-only` container agree on it without further configuration.
RUN set -eu; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates; \
    rm -rf /var/lib/apt/lists/*; \
    groupadd --gid 10001 flux; \
    useradd --uid 10001 --gid 10001 --home-dir /srv/flux \
            --no-create-home --shell /usr/sbin/nologin flux; \
    install -d -o 10001 -g 10001 -m 0750 /srv/flux; \
    install -d -o 10001 -g 10001 -m 0750 /srv/flux/workspace; \
    install -d -m 0555 /run/flux-tls

COPY flux /usr/local/bin/flux

USER 10001:10001
WORKDIR /srv/flux
EXPOSE 8790

# Only `flux system serve`. There is no shell wrapper and no init: the bearer token arrives through
# the environment (`FLUX_REMOTE_SYSTEM_TOKEN`, or whatever `--token-env` names), so nothing has to
# read a file before exec.
ENTRYPOINT ["/usr/local/bin/flux", "system", "serve"]

# The mount points the profile documents. Override by passing your own arguments after the image
# reference; they replace this list wholesale.
#
# `--no-sandbox` is deliberate and load-bearing. A serving surface defaults to a fail-closed
# `require` sandbox floor, and bubblewrap cannot satisfy it here: creating a user namespace inside
# an ordinary container is refused, so an image that relied on it would fail to start on any host
# with the default seccomp profile. `--no-sandbox` is that floor's documented "outer container/VM"
# escape — the container is the isolation boundary, which is why the profiles run it read-only,
# non-root, with all capabilities dropped and ingress restricted. The daemon prints a prominent
# UNCONFINED posture at startup; that line is the contract, not noise. An operator who overrides
# this CMD and drops the flag gets a daemon that refuses to start rather than one that quietly runs
# unconfined.
CMD ["--no-sandbox", \
     "--workspace", "/srv/flux/workspace", \
     "--bind", "0.0.0.0:8790", \
     "--cert", "/run/flux-tls/tls.crt", \
     "--key", "/run/flux-tls/tls.key"]
