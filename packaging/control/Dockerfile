# illogical-control, as the hosted one runs: the static binary (`just static`)
# on distroless, which brings CA certificates (for GitHub's API) and nothing
# else. It runs as root: Fly volumes are root-owned. `just control-deploy`
# builds the context and deploys to Fly.
FROM gcr.io/distroless/static-debian12
COPY illogical-control /illogical-control
# The daemon hosted sandboxes run (M20).
COPY illogicald /illogicald
EXPOSE 7690
ENTRYPOINT ["/illogical-control"]
