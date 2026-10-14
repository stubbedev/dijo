# syntax=docker/dockerfile:1

# The statically linked executable (`just static`, or the binary of a release's
# <arch>-linux-static archive) in an image with nothing else: build it from a directory holding
# only that executable, named extractous, as `just container` does.
FROM alpine:3.22@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8 AS layout
COPY extractous /rootfs/extractous
RUN mkdir -p /rootfs/tmp /rootfs/var/cache/extractous \
    && chmod 1777 /rootfs/tmp \
    && chown 10001:10001 /rootfs/var/cache/extractous \
    && printf 'probe\n' > /tmp/probe.txt \
    && /rootfs/extractous detect /tmp/probe.txt

FROM scratch
COPY --from=layout /rootfs/ /
ENV XDG_CACHE_HOME=/var/cache \
    EXTRACTOUS_LISTEN=0.0.0.0:9998
USER 10001:10001
EXPOSE 9998
ENTRYPOINT ["/extractous"]
CMD ["serve"]
