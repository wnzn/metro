#!/bin/sh
# Install the same checksum-pinned Linux x86-64 cosign in signing and verification jobs.
set -eu
if [ "$#" -ne 1 ] || [ -e "$1" ] || [ -L "$1" ]; then
    echo "usage: install_cosign.sh NEW_OUTPUT_PATH" >&2
    exit 1
fi

version=v3.1.4
sha256=424e2c97d0b45fd7a6b608a87f090f35633f4c23ece5f092d48bba11aaf4bbfa
file=$(mktemp "${1}.XXXXXX")
trap 'rm -f -- "$file"' EXIT
curl --fail --silent --show-error --location --retry 3 --output "$file" \
    "https://github.com/sigstore/cosign/releases/download/$version/cosign-linux-amd64"
printf '%s  %s\n' "$sha256" "$file" | sha256sum --check -
chmod 0755 "$file"
mv -T -- "$file" "$1"
