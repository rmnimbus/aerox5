#!/bin/sh
# Build aerox5-<version>-x86_64.AppImage with its windowing libraries bundled.
#
# Bundled: libwayland-*, libxkbcommon(-x11), libX11 + friends (X11 fallback) and everything
# they link to. NOT bundled (AppImage excludelist practice): glibc, libgcc_s/libstdc++,
# libGL/libEGL/libdrm/libgbm (the GPU driver stack must match the host's kernel driver).
#
# Tools are pinned release binaries (Gitea mirrors: mirrors/appimagetool, mirrors/type2-runtime;
# release assets aren't mirrored, so they're fetched from GitHub and checked by SHA-256).
#
# Usage: packaging/build-appimage.sh            (from the repo root; output in dist/)
set -eu

APPIMAGETOOL_URL=https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
APPIMAGETOOL_SHA=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
RUNTIME_URL=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
RUNTIME_SHA=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

# Loaded with dlopen() by winit/glutin (so ldd on the binary doesn't show them).
DLOPENED="libwayland-client.so.0 libwayland-cursor.so.0 libwayland-egl.so.1
libxkbcommon.so.0 libxkbcommon-x11.so.0
libX11.so.6 libX11-xcb.so.1 libXcursor.so.1 libXrandr.so.2 libXi.so.6 libXext.so.6 libXrender.so.1 libXfixes.so.3"

# Never bundle these (host-provided base system / GPU drivers).
EXCLUDE='^(linux-vdso|ld-linux[-a-z0-9_]*|libc|libm|libdl|libpthread|librt|libresolv|libutil|libgcc_s|libstdc\+\+|libGL|libGLX|libGLdispatch|libEGL|libOpenGL|libdrm|libgbm|libexpat|libz|libfontconfig|libfreetype|libharfbuzz)\.so'

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CACHE=${XDG_CACHE_HOME:-$HOME/.cache}/aerox5-build
mkdir -p "$CACHE" "$ROOT/dist"

fetch() { # url sha dest
    if [ ! -f "$3" ] || ! echo "$2  $3" | sha256sum -c --quiet - 2>/dev/null; then
        echo "fetching $1"
        curl -fsSL -o "$3.part" "$1"
        echo "$2  $3.part" | sha256sum -c --quiet -
        mv "$3.part" "$3"
    fi
}
fetch "$APPIMAGETOOL_URL" "$APPIMAGETOOL_SHA" "$CACHE/appimagetool"
fetch "$RUNTIME_URL" "$RUNTIME_SHA" "$CACHE/runtime-x86_64"
chmod +x "$CACHE/appimagetool"

cd "$ROOT"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release --locked
BIN=${CARGO_TARGET_DIR:-target}/release/aerox5

APPDIR=$(mktemp -d)/AppDir
trap 'rm -rf "$(dirname "$APPDIR")"' EXIT
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/scalable/apps"
install -m 0755 "$BIN" "$APPDIR/usr/bin/aerox5"
install -m 0644 packaging/aerox5.desktop "$APPDIR/aerox5.desktop"
install -m 0644 packaging/aerox5.desktop "$APPDIR/usr/share/applications/aerox5.desktop"
install -m 0644 packaging/aerox5.svg "$APPDIR/aerox5.svg"
install -m 0644 packaging/aerox5.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/aerox5.svg"
ln -s aerox5.svg "$APPDIR/.DirIcon"

resolve() { # soname -> path on this system
    ldconfig -p | awk -v n="$1" '$1 == n && /x86-64/ { print $NF; exit }'
}

# Breadth-first: the dlopened libs, then everything they (and the binary) link to.
queue=$DLOPENED
for l in $(ldd "$BIN" | awk '/=>/ { n = $1; sub(".*/", "", n); print n }'); do queue="$queue $l"; done
seen=" "
while [ -n "$(echo $queue)" ]; do
    set -- $queue; lib=$1; shift; queue="$*"
    case "$seen" in *" $lib "*) continue ;; esac
    seen="$seen$lib "
    echo "$lib" | grep -Eq "$EXCLUDE" && continue
    path=$(resolve "$lib")
    if [ -z "$path" ]; then echo "missing on build host: $lib" >&2; exit 1; fi
    cp -L "$path" "$APPDIR/usr/lib/$lib"
    for dep in $(ldd "$path" | awk '/=>/ { n = $1; sub(".*/", "", n); print n }'); do queue="$queue $dep"; done
done

# libxkbcommon reads keymaps/Compose tables from the host's /usr/share; an older bundled copy
# chokes on newer host data. Use the host's copy when it has one, the bundled one otherwise.
mkdir -p "$APPDIR/usr/lib/hostfirst"
mv "$APPDIR/usr/lib/libxkbcommon.so.0" "$APPDIR/usr/lib/hostfirst/"

cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE=$(dirname "$(readlink -f "$0")")
host_has() {
    for d in /usr/lib64 /usr/lib/x86_64-linux-gnu /lib/x86_64-linux-gnu /usr/lib /lib64 /run/current-system/profile/lib; do
        [ -e "$d/$1" ] && return 0
    done
    return 1
}
LIBS="$HERE/usr/lib"
host_has libxkbcommon.so.0 || LIBS="$LIBS:$HERE/usr/lib/hostfirst"
export LD_LIBRARY_PATH="$LIBS${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/usr/bin/aerox5" "$@"
EOF
chmod 0755 "$APPDIR/AppRun"

echo "bundled libraries:"; (cd "$APPDIR/usr/lib" && find . -name "*.so*" | sort | sed 's|^\./|  |')
# Oldest glibc this AppImage can run on.
need=$(for f in "$APPDIR/usr/bin/aerox5" $(find "$APPDIR/usr/lib" -name "*.so*"); do objdump -T "$f"; done \
       | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)
echo "requires $need or newer"

OUT="$ROOT/dist/aerox5-$VERSION-x86_64.AppImage"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$CACHE/appimagetool" --no-appstream \
    --runtime-file "$CACHE/runtime-x86_64" "$APPDIR" "$OUT"
( cd "$ROOT/dist" && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256" )
echo "built $OUT"
