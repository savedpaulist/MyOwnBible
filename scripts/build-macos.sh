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
app="$bundle_root/BibleOWN.app"
contents="$app/Contents"

if [ ! -f "$binary" ]; then
  echo "Built binary not found: $binary" >&2
  exit 1
fi

rm -rf "$app"
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

# Ad-hoc signing validates the locally transferred beta bundle. Public
# distribution still requires Developer ID signing and notarization.
if command -v codesign >/dev/null 2>&1; then
  codesign --force --deep --sign - "$app"
fi

archive="$bundle_root/BibleOWN-macos-10.15-$bundle_arch.zip"
rm -f "$archive"
ditto -c -k --sequesterRsrc --keepParent "$app" "$archive"

echo
echo "Application bundle created:"
echo "  $app"
echo "Transfer archive:"
echo "  $archive"
echo
echo "Open it in Finder. It does not launch a Terminal window."
if [ -n "$target" ]; then
  echo "This bundle is Intel x86_64 and is suitable for macOS 10.15 Catalina."
fi
