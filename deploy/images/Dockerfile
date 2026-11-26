# Build the patched peri-swarm gateway. The repository pins Rust 1.99.0 and
# requires scripts/cargo-rmcp-patched.sh for the rmcp/hyper-util patches.
FROM rust:1.99.0-bookworm AS rust-build

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        cmake \
        curl \
        patch \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# The target cache mount is not part of the image; copy the binary to /out in
# the same layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    ./scripts/cargo-rmcp-patched.sh build --locked --release -p peri-swarm \
    && mkdir -p /out \
    && cp target/release/peri-swarm /out/

FROM debian:bookworm-slim AS peri-swarm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 peri \
    && useradd --uid 10001 --gid 10001 --home-dir /var/lib/peri-swarm \
        --create-home --shell /usr/sbin/nologin peri \
    && mkdir -p /var/lib/peri-swarm/.peri /var/lib/peri-swarm/workspace \
    && chown -R peri:peri /var/lib/peri-swarm \
    && chmod 700 /var/lib/peri-swarm/.peri

COPY --from=rust-build /out/peri-swarm /usr/local/bin/peri-swarm
COPY deploy/images/peri-swarm-entrypoint.sh /usr/local/bin/peri-swarm-entrypoint.sh

RUN chmod 0755 /usr/local/bin/peri-swarm /usr/local/bin/peri-swarm-entrypoint.sh

USER 10001:10001
ENV HOME=/var/lib/peri-swarm
EXPOSE 8080

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/peri-swarm-entrypoint.sh"]
