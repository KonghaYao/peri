FROM alpine:3.22

# Exercise Git-enabled discovery; plain alpine also tests directory sessions without Git.
RUN apk add --no-cache git bash
