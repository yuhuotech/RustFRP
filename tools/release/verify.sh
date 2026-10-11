#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: bash tools/release/verify.sh <trusted-public-key.pem> <release-directory>" >&2
  exit 2
fi

PUBLIC_KEY="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
RELEASE_DIR="$(cd "$2" && pwd)"
openssl pkeyutl -verify -pubin -inkey "$PUBLIC_KEY" -rawin \
  -in "$RELEASE_DIR/SHA256SUMS" -sigfile "$RELEASE_DIR/SHA256SUMS.sig"
(cd "$RELEASE_DIR" && sha256sum --check --strict SHA256SUMS)
