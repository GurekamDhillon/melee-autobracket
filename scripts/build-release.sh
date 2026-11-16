#!/usr/bin/env bash
# Builds the release archive for this machine's target:
#
#   dist/autobracket-<version>-<target>.tar.gz   autobracket, autobracket-client (stripped),
#                                                README.md, LICENSE, deploy/
#   dist/SHA256SUMS                              its SHA-256
#
# <version> is the workspace version (Cargo.toml), <target> the host triple from `rustc -vV`.
# Running it again rebuilds the same archive in place. Refuses to run with uncommitted changes
# unless --allow-dirty is given, so an archive always matches a commit.
set -euo pipefail

usage() {
    echo "usage: scripts/build-release.sh [--allow-dirty]" >&2
}

allow_dirty=0
for arg in "$@"; do
    case "$arg" in
        --allow-dirty) allow_dirty=1 ;;
        -h | --help) usage; exit 0 ;;
        *) usage; exit 2 ;;
    esac
done

cd "$(dirname "$0")/.."

if [ "$allow_dirty" = 0 ] && [ -n "$(git status --porcelain)" ]; then
    echo "error: the working tree has uncommitted changes; commit them, or pass --allow-dirty" >&2
    exit 1
fi

# autobracket-server takes its version from [workspace.package]; pkgid ends in "@<version>".
version="$(cargo pkgid -p autobracket-server)"
version="${version##*@}"
target="$(rustc -vV | sed -n 's/^host: //p')"
name="autobracket-${version}-${target}"

cargo build --release --locked --bin autobracket --bin autobracket-client

stage="dist/${name}"
rm -rf "$stage" "dist/${name}.tar.gz" dist/SHA256SUMS
mkdir -p "$stage/deploy"
for bin in autobracket autobracket-client; do
    install -m 755 "target/release/${bin}" "$stage/"
    strip "$stage/${bin}"
done
install -m 644 README.md LICENSE "$stage/"
install -m 644 \
    deploy/autobracket.example.toml \
    deploy/autobracket.service \
    deploy/docker-compose.yml \
    deploy/Caddyfile.example \
    deploy/nginx.conf.example \
    "$stage/deploy/"

# Fixed order, owner and times (the commit's), so the same commit gives the same archive.
mtime="$(git log -1 --format=%ct)"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@${mtime}" \
    -C dist -cf - "$name" | gzip -n -9 > "dist/${name}.tar.gz"
rm -rf "$stage"
(cd dist && sha256sum "${name}.tar.gz" > SHA256SUMS)

echo "dist/${name}.tar.gz ($(du -h "dist/${name}.tar.gz" | cut -f1))"
cat dist/SHA256SUMS
