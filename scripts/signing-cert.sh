#!/bin/sh
# Creates "RustyBar Local", a self-signed code signing certificate in the login keychain, which
# scripts/bundle.sh then signs with. macOS ties the permissions it grants to the signature: with
# this one they survive rebuilds, while an ad hoc signature changes at every build. Run it once;
# macOS asks for your password to trust the certificate for code signing.
set -eu

name="RustyBar Local"
if security find-identity -v -p codesigning | grep -q "\"$name\""; then
  echo "$name is already in the keychain"
  exit 0
fi

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
cat > "$dir/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF

# The openssl of macOS (LibreSSL) writes a PKCS#12 file that security can import
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$dir/cert.cnf" \
  -keyout "$dir/key.pem" -out "$dir/cert.pem" 2>/dev/null
/usr/bin/openssl pkcs12 -export -inkey "$dir/key.pem" -in "$dir/cert.pem" -out "$dir/cert.p12" \
  -passout pass:rustybar
keychain="$HOME/Library/Keychains/login.keychain-db"
security import "$dir/cert.p12" -k "$keychain" -P rustybar -T /usr/bin/codesign
security add-trusted-cert -r trustRoot -p codeSign -k "$keychain" "$dir/cert.pem"
echo "created $name: scripts/bundle.sh signs with it from now on"
