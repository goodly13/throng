#!/usr/bin/env bash
# Build throng's installers for the platform this runs on, into dist/.
#
#   Linux    throng_<version>_<arch>.deb
#            throng-<version>-<machine>.AppImage   (when appimagetool is on PATH or $APPIMAGETOOL)
#            throng-<version>-linux-<machine>.tar.gz
#   macOS    throng-<version>-macos-universal.dmg  (throng.app for Apple silicon and Intel)
#            Signed with $APPLE_SIGNING_IDENTITY and notarised with $APPLE_ID, $APPLE_TEAM_ID and
#            $APPLE_APP_PASSWORD when those are set; ad-hoc signed otherwise, which runs on the
#            machine that built it but is refused by Gatekeeper elsewhere.
#   Windows  throng-<version>-windows-<machine>.zip
#            throng-<version>-windows-<machine>.msi    (when WiX v5's `wix` is on PATH)
#
# Usage: packaging/package.sh [--skip-build]   (--skip-build packages what target/ already holds)
#
# This builds packages; it publishes nothing.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
dist="$root/dist"
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$root/Cargo.toml")
[ -n "$version" ] || { echo "package.sh: no version in Cargo.toml" >&2; exit 1; }
skip_build=false
[ "${1:-}" = "--skip-build" ] && skip_build=true
machine=$(uname -m)
mkdir -p "$dist"
cd "$root"

build() {
  $skip_build && return 0
  if [ -n "${1:-}" ]; then
    cargo build --release --locked -p throng-app --target "$1"
  else
    cargo build --release --locked -p throng-app
  fi
}

# The licence and the notice of what this work derives from travel with every package.
# (mkdir and cp rather than `install -D`, which macOS's install lacks.)
legal() {
  mkdir -p "$1"
  cp "$root/LICENSE" "$root/NOTICE" "$1/"
}

# The newest glibc symbol version the binary needs: the oldest glibc it runs on.
glibc_floor() {
  command -v objdump >/dev/null || return 0
  objdump -T "$1" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -V | tail -1
}

linux() {
  build
  local bin="$root/target/release/throng"
  local arch; arch=$(dpkg --print-architecture 2>/dev/null || echo "$machine")

  # .deb — the GUI libraries are loaded at run time (X11 or Wayland, and OpenGL), so they are
  # declared here rather than found by the linker.
  local deb="$dist/deb"
  rm -rf "$deb"
  install -Dm755 "$bin" "$deb/usr/bin/throng"
  install -Dm644 "$here/linux/throng.desktop" "$deb/usr/share/applications/throng.desktop"
  install -Dm644 "$here/throng.png" "$deb/usr/share/icons/hicolor/256x256/apps/throng.png"
  legal "$deb/usr/share/doc/throng"
  local libc="libc6"
  local floor; floor=$(glibc_floor "$bin")
  [ -n "$floor" ] && libc="libc6 (>= $floor)"
  mkdir -p "$deb/DEBIAN"
  cat > "$deb/DEBIAN/control" <<EOF
Package: throng
Version: $version
Architecture: $arch
Maintainer: throng contributors
Installed-Size: $(du -sk "$deb/usr" | cut -f1)
Depends: $libc, libgl1, libegl1, libxkbcommon0, libxkbcommon-x11-0, libx11-6, libx11-xcb1, libxcursor1, libxrandr2, libxi6
Recommends: libwayland-client0, libwayland-egl1
Section: devel
Priority: optional
Homepage: https://github.com/goodly13/throng
Description: Project-first terminal and agent workspace
 throng keeps each project's terminals, editors and file tree together, and
 keeps terminals running when the window closes.
EOF
  dpkg-deb --build --root-owner-group "$deb" "$dist/throng_${version}_${arch}.deb"
  rm -rf "$deb"

  # AppImage. The daemon is started as "$APPIMAGE daemon", so it holds its own mount.
  local appdir="$dist/AppDir"
  rm -rf "$appdir"
  install -Dm755 "$bin" "$appdir/usr/bin/throng"
  install -Dm644 "$here/linux/throng.desktop" "$appdir/throng.desktop"
  install -Dm644 "$here/throng.png" "$appdir/throng.png"
  install -Dm644 "$here/throng.png" "$appdir/usr/share/icons/hicolor/256x256/apps/throng.png"
  legal "$appdir/usr/share/doc/throng"
  cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
exec "$here/usr/bin/throng" "$@"
EOF
  chmod 755 "$appdir/AppRun"
  local tool="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
  if [ -n "$tool" ]; then
    APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$machine" "$tool" --no-appstream "$appdir" \
      "$dist/throng-$version-$machine.AppImage"
  else
    echo "package.sh: appimagetool not found; no AppImage" >&2
  fi
  rm -rf "$appdir"

  # A plain archive for everything else.
  local stage="$dist/throng-$version-linux-$machine"
  rm -rf "$stage"
  install -Dm755 "$bin" "$stage/throng"
  install -Dm644 "$here/linux/throng.desktop" "$stage/throng.desktop"
  install -Dm644 "$here/throng.png" "$stage/throng.png"
  legal "$stage"
  tar -C "$dist" -czf "$stage.tar.gz" "$(basename "$stage")"
  rm -rf "$stage"
}

macos() {
  local targets=(aarch64-apple-darwin x86_64-apple-darwin)
  local bins=()
  for target in "${targets[@]}"; do
    $skip_build || rustup target add "$target" >/dev/null
    build "$target"
    bins+=("$root/target/$target/release/throng")
  done
  local app="$dist/throng.app"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  lipo -create -output "$app/Contents/MacOS/throng" "${bins[@]}"
  sed "s/@VERSION@/$version/g" "$here/macos/Info.plist" > "$app/Contents/Info.plist"
  legal "$app/Contents/Resources"

  local iconset="$dist/throng.iconset"
  rm -rf "$iconset"
  mkdir -p "$iconset"
  for size in 16 32 64 128 256; do
    sips -z "$size" "$size" "$here/throng.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    local double=$((size * 2))
    sips -z "$double" "$double" "$here/throng.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$iconset" -o "$app/Contents/Resources/throng.icns"
  rm -rf "$iconset"

  if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
    codesign --force --options runtime --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$app"
  else
    codesign --force --sign - "$app"
  fi
  codesign --verify --strict "$app"

  local staging="$dist/dmg"
  local dmg="$dist/throng-$version-macos-universal.dmg"
  rm -rf "$staging" "$dmg"
  mkdir -p "$staging"
  cp -R "$app" "$staging/"
  ln -s /Applications "$staging/Applications"
  hdiutil create -volname "throng $version" -srcfolder "$staging" -ov -format UDZO "$dmg" >/dev/null
  rm -rf "$staging" "$app"

  if [ -n "${APPLE_SIGNING_IDENTITY:-}" ] && [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] &&
    [ -n "${APPLE_APP_PASSWORD:-}" ]; then
    codesign --force --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$dmg"
    xcrun notarytool submit "$dmg" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
      --password "$APPLE_APP_PASSWORD" --wait
    xcrun stapler staple "$dmg"
  else
    echo "package.sh: no Apple signing identity and notarisation account; the .dmg is not notarised" >&2
  fi
}

windows() {
  build
  local name="throng-$version-windows-$machine"
  local stage="$dist/$name"
  rm -rf "$stage" "$dist/$name.zip"
  mkdir -p "$stage"
  cp "$root/target/release/throng.exe" "$stage/"
  cp "$root/README.md" "$stage/README.md"
  legal "$stage"
  powershell -NoProfile -Command \
    "Compress-Archive -Path '$(cygpath -w "$stage")\\*' -DestinationPath '$(cygpath -w "$dist/$name.zip")' -Force"
  rm -rf "$stage"

  # A per-user installer.
  if command -v wix >/dev/null; then
    wix build "$(cygpath -w "$here/windows/throng.wxs")" -arch x64 \
      -d "Version=$version" -d "BinDir=$(cygpath -w "$root/target/release")" -d "Root=$(cygpath -w "$root")" \
      -o "$(cygpath -w "$dist/$name.msi")"
  else
    echo "package.sh: wix not found; no .msi" >&2
  fi
}

case "$(uname -s)" in
  Linux) linux ;;
  Darwin) macos ;;
  MINGW* | MSYS* | CYGWIN*) windows ;;
  *) echo "package.sh: no packaging for $(uname -s)" >&2; exit 1 ;;
esac
ls -l "$dist"
