# autobracket server image: the `autobracket` binary (serve, admin, keygen, init,
# verify-worker) on Debian 13 slim, running as an unprivileged user with its state in /data.
# It contains neither the game engine nor any disc image; mount your own read-only to enable
# re-simulation (see deploy/docker-compose.yml).
#
#   docker build -t autobracket:local .
#   docker run --rm -v autobracket-data:/data autobracket:local init --dir /data --bind 0.0.0.0:8080
#   docker run -d -v autobracket-data:/data -p 127.0.0.1:8080:8080 autobracket:local

# The workspace's rust-version (Cargo.toml). Builder and runtime share one Debian release, so
# the binary's glibc matches.
ARG RUST_VERSION=1.94
ARG DEBIAN_RELEASE=trixie

FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS build
WORKDIR /src
COPY . .
# aws-lc-sys (TLS) and the bundled SQLite compile C code with the image's gcc. Build, keep the
# stripped binary, and drop the build tree and the registry in the same layer so neither stays
# in the build cache.
RUN cargo build --release --locked -p autobracket-server --bin autobracket \
    && install -D -m 755 -s target/release/autobracket /out/autobracket \
    && rm -rf target "$CARGO_HOME/registry" "$CARGO_HOME/git"

FROM debian:${DEBIAN_RELEASE}-slim
ARG UID=10001
LABEL org.opencontainers.image.title="autobracket" \
      org.opencontainers.image.description="Self-hosted Melee tournament bracket server that advances only on replay-verified results" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.source="https://github.com/GurekamDhillon/melee-autobracket"
# libasound2t64 and libudev1 are not used by autobracket; the melee engine links them, so a
# mounted engine binary runs here for the in_process verifier. autobracket links no OpenSSL
# (TLS is rustls; the base image's own libssl is there only for apt and coreutils), and the
# ca-certificates package would add the openssl tool, so the CA bundle `admin` and
# `verify-worker` need for https is copied from the build stage instead.
RUN apt-get update \
    && apt-get install -y --no-install-recommends libasound2t64 libudev1 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid "$UID" --user-group --home-dir /data --no-create-home \
       --shell /usr/sbin/nologin autobracket \
    && install -d -m 700 -o autobracket -g autobracket /data
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /out/autobracket /usr/local/bin/autobracket
COPY LICENSE README.md /usr/share/doc/autobracket/
COPY deploy/autobracket.example.toml /usr/share/doc/autobracket/
USER ${UID}:${UID}
WORKDIR /data
VOLUME /data
EXPOSE 8080
ENV AUTOBRACKET_LOG=info
ENTRYPOINT ["autobracket"]
CMD ["serve", "--config", "/data/autobracket.toml"]
