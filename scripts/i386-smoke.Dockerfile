FROM alpine:3.22

# Workspace discovery and shell tools are external processes, even in a static build.
RUN apk add --no-cache git bash
