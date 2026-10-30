#!/bin/sh
# Builds target/RustyBar.app: the release binary in an app bundle, signed.
#   scripts/bundle.sh              for this Mac
#   scripts/bundle.sh --universal  for Apple silicon and Intel (needs both Rust targets)
# Signing: macOS ties the permissions it grants (Calendars, Accessibility…) to the signature. An
# ad hoc signature changes at every build, so macOS would ask again each time: with a stable
# identity it doesn't. RUSTYBAR_SIGN_IDENTITY picks one (e.g. a Developer ID), else a certificate
# named "RustyBar Local" in the keychain (scripts/signing-cert.sh makes one), else ad hoc.
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
app=target/RustyBar.app

if [ "${1:-}" = --universal ]; then
  cargo build --release --target aarch64-apple-darwin --target x86_64-apple-darwin
  mkdir -p target/universal
  lipo -create -output target/universal/rustybar \
    target/aarch64-apple-darwin/release/rustybar target/x86_64-apple-darwin/release/rustybar
  binary=target/universal/rustybar
else
  cargo build --release
  binary=target/release/rustybar
fi

rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$binary" "$app/Contents/MacOS/rustybar"
sed "s/@VERSION@/$version/g" resources/Info.plist > "$app/Contents/Info.plist"

identity="${RUSTYBAR_SIGN_IDENTITY:-}"
if [ -z "$identity" ] && security find-identity -v -p codesigning | grep -q '"RustyBar Local"'; then
  identity="RustyBar Local"
fi
codesign --force --sign "${identity:--}" --identifier io.github.francesco-rapetti.rustybar "$app"
echo "$app ($version, signed ${identity:-ad hoc})"
