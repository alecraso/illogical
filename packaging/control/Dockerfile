# arugula-control, as the hosted one runs: the static binary (`just static`)
# on distroless, which brings CA certificates (for GitHub's API) and nothing
# else. It runs as root: Fly volumes are root-owned. `just control-deploy`
# builds the context and deploys to Fly.

# Litestream (#174), for the off-site backup: a static binary, checked
# against the release's checksum. Control runs it only when LITESTREAM_BUCKET
# is set (docs/control.md, "Operating the hosted one").
FROM alpine:3.22 AS litestream
ARG LITESTREAM=0.5.17
ARG LITESTREAM_SHA256=cfb371176d164437ae869f8351cfde49bd1804ae71c61923f75c9cba9c9c006d
RUN wget -qO /ls.tgz "https://github.com/benbjohnson/litestream/releases/download/v${LITESTREAM}/litestream-${LITESTREAM}-linux-x86_64.tar.gz" \
 && echo "${LITESTREAM_SHA256}  /ls.tgz" | sha256sum -c - \
 && tar -xzf /ls.tgz -C / litestream

FROM gcr.io/distroless/static-debian12
COPY arugula-control /arugula-control
# The daemon hosted sandboxes run (M20).
COPY arugulad /arugulad
COPY --from=litestream /litestream /litestream
EXPOSE 7690
ENTRYPOINT ["/arugula-control"]
