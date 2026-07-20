# Workaround for an upstream Arch packaging bug (as of 2026-07-20).
#
# ghcr.io/arch-network/local_validator:latest ships a binary built against GLIBC 2.38
# on a Debian 12 "bookworm" base that only provides GLIBC 2.36, so the container
# dies instantly with:
#   /bin/local_validator: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.38' not found
#
# Pinning to an older tag (0.2.16) is not an option: that build predates the
# --titan-endpoint flag that arch-cli 0.6.7 passes.
#
# So: take the correct binary from :latest and run it on Debian 13 "trixie",
# which provides a new enough GLIBC. Entrypoint matches the original image.

FROM ghcr.io/arch-network/local_validator:latest AS upstream

FROM debian:trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=upstream /bin/local_validator /bin/local_validator

RUN /bin/local_validator --version

ENTRYPOINT ["tini", "--"]
CMD ["local_validator"]
