#!/bin/sh
set -eu

version="$(sw_vers -productVersion)"
major="${version%%.*}"
rest="${version#*.}"
minor="${rest%%.*}"

if [ "$major" -lt 10 ] || { [ "$major" -eq 10 ] && [ "$minor" -lt 15 ]; }; then
  echo "BibleOWN requires macOS 10.15 Catalina or newer (found $version)." >&2
  exit 1
fi

export MACOSX_DEPLOYMENT_TARGET=10.15
if [ "$major" -eq 10 ] && [ "$minor" -eq 15 ]; then
  echo "Building the Catalina-compatible BibleOWN profile (legacy file-type API)."
else
  echo "Building BibleOWN for macOS $version with a Catalina 10.15 deployment target."
fi

if [ "${1:-}" = "--bundle" ]; then
  shift
  exec cargo bundle "$@"
fi

exec cargo build "$@"
