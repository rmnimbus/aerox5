#!/bin/sh
# Build the AppImage inside Debian 12 (glibc 2.36) so it runs on older distros too
# (building on a rolling distro would require that distro's brand-new glibc).
# Needs podman (rootless is fine) or docker. Output: dist/aerox5-<version>-x86_64.AppImage
set -eu
IMAGE=docker.io/library/rust:1.99.1-bookworm
ROOT=$(cd "$(dirname "$0")/.." && pwd)
ENGINE=$(command -v podman || command -v docker)
CACHE=${XDG_CACHE_HOME:-$HOME/.cache}/aerox5-build
mkdir -p "$CACHE/cargo-registry" "$CACHE/target"

"$ENGINE" run --rm \
    -v "$ROOT:/src" \
    -v "$CACHE:/cache" \
    -v "$CACHE/cargo-registry:/usr/local/cargo/registry" \
    -e XDG_CACHE_HOME=/cache -e CARGO_TARGET_DIR=/cache/target \
    -w /src "$IMAGE" sh -euc '
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends \
            libwayland-client0 libwayland-cursor0 libwayland-egl1 \
            libxkbcommon0 libxkbcommon-x11-0 libx11-6 libx11-xcb1 \
            libxcursor1 libxrandr2 libxi6 libxext6 libxrender1 libxfixes3 \
            binutils file >/dev/null
        HOME=/tmp sh packaging/build-appimage.sh
    '
