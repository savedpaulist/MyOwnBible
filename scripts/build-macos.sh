#!/bin/sh
set -eu

bundle=false
target=""

# Script-specific switches must precede ordinary Cargo arguments.
while [ "$#" -gt 0 ]; do
  case "$1" in
    --bundle)
      bundle=true
      shift
      ;;
    --intel-catalina)
      target="x86_64-apple-darwin"
      shift
      ;;
    *)
      break
      ;;
  esac
done

system_version="$(sw_vers -productVersion)"
major="${system_version%%.*}"
rest="${system_version#*.}"
minor="${rest%%.*}"

if [ "$major" -lt 10 ] || { [ "$major" -eq 10 ] && [ "$minor" -lt 15 ]; }; then
  echo "BibleOWN requires macOS 10.15 Catalina or newer (found $system_version)." >&2
  exit 1
fi

export MACOSX_DEPLOYMENT_TARGET=10.15
# Some linker/toolchain combinations ignore the environment variable for the
# final executable. Pass the deployment target to ld as well so LC_BUILD_VERSION
# cannot silently stay at 11.0 when building on a newer SDK.
if [ -n "${RUSTFLAGS:-}" ]; then
  export RUSTFLAGS="$RUSTFLAGS -C link-arg=-mmacosx-version-min=10.15"
else
  export RUSTFLAGS="-C link-arg=-mmacosx-version-min=10.15"
fi

if [ -n "$target" ] && ! rustup target list --installed | grep -qx "$target"; then
  echo "Installing the Rust target required for an Intel Catalina build: $target"
  rustup target add "$target"
fi

release=false
for argument in "$@"; do
  if [ "$argument" = "--release" ]; then
    release=true
  fi
done

if [ -n "$target" ]; then
  echo "Building an Intel x86_64 application for macOS 10.15 Catalina."
  cargo build --target "$target" "$@"
  output_root="target/$target"
  bundle_arch="x86_64"
else
  host_arch="$(uname -m)"
  echo "Building for this Mac ($host_arch) with a macOS 10.15 deployment target."
  if [ "$host_arch" = "arm64" ]; then
    echo "Note: this build will not run on an Intel Catalina Mac. Use --intel-catalina for that."
  fi
  cargo build "$@"
  output_root="target"
  bundle_arch="$host_arch"
fi

if [ "$bundle" != true ]; then
  exit 0
fi

if [ "$release" = true ]; then
  profile="release"
else
  profile="debug"
fi

binary="$output_root/$profile/bibleown"
bundle_root="$output_root/$profile/bundle/osx"
app="$bundle_root/BibleOWN-Catalina.app"
contents="$app/Contents"

if [ ! -f "$binary" ]; then
  echo "Built binary not found: $binary" >&2
  exit 1
fi

binary_minos="$({
  otool -l "$binary" | awk '
    $1 == "cmd" && $2 == "LC_BUILD_VERSION" { build = 1; legacy = 0; next }
    $1 == "cmd" && $2 == "LC_VERSION_MIN_MACOSX" { legacy = 1; build = 0; next }
    build && $1 == "minos" { print $2; exit }
    legacy && $1 == "version" { print $2; exit }
  '
} || true)"

if [ -z "$binary_minos" ]; then
  echo "Cannot determine the Mach-O minimum macOS version for $binary." >&2
  exit 1
fi

min_major="${binary_minos%%.*}"
min_rest="${binary_minos#*.}"
min_minor="${min_rest%%.*}"
if [ "$min_major" -gt 10 ] || { [ "$min_major" -eq 10 ] && [ "$min_minor" -gt 15 ]; }; then
  echo "Refusing to package $binary: Mach-O minimum is macOS $binary_minos, not Catalina." >&2
  echo "Remove the target directory and run this script again." >&2
  exit 1
fi

# Remove the old cargo-bundle output as well, so Finder cannot accidentally
# open a stale package whose Info.plist still says macOS 11.
rm -rf "$bundle_root/BibleOWN.app" "$app"
mkdir -p "$contents/MacOS" "$contents/Resources"
cp "$binary" "$contents/MacOS/bibleown"
chmod 755 "$contents/MacOS/bibleown"
cp "resources/branding/macos/mob.icns" "$contents/Resources/mob.icns"

cat > "$contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>BibleOWN</string>
  <key>CFBundleExecutable</key>
  <string>bibleown</string>
  <key>CFBundleIconFile</key>
  <string>mob.icns</string>
  <key>CFBundleIdentifier</key>
  <string>com.bibleown.desktop</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>BibleOWN</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSMinimumSystemVersion</key>
  <string>10.15</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

plist_minos="$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$contents/Info.plist")"
if [ "$plist_minos" != "10.15" ]; then
  echo "Refusing to package: Info.plist minimum is $plist_minos, not 10.15." >&2
  exit 1
fi

# Ad-hoc signing validates the locally transferred beta bundle. Public
# distribution still requires Developer ID signing and notarization.
if command -v codesign >/dev/null 2>&1; then
  codesign --force --deep --sign - "$app"
fi

archive="$bundle_root/BibleOWN-Catalina-$bundle_arch.zip"
rm -f "$archive"
ditto -c -k --sequesterRsrc --keepParent "$app" "$archive"

echo
echo "Application bundle created:"
echo "  $app"
echo "Transfer archive:"
echo "  $archive"
echo
echo "Open it in Finder. It does not launch a Terminal window."
echo "Verified Mach-O minimum: macOS $binary_minos"
echo "Verified Info.plist minimum: macOS $plist_minos"
if [ -n "$target" ]; then
  echo "This bundle is Intel x86_64 and is suitable for macOS 10.15 Catalina."
fi
