#!/bin/sh
# Fails unless vendor/twilight-model is exactly the crates.io release in vendor/twilight-model.lock
# plus vendor/twilight-model.patch. On a twilight upgrade: update the version and checksum there,
# re-apply the change, and regenerate the patch (see the comment in Cargo.toml).
set -eu
cd "$(dirname "$0")/.."
lock=vendor/twilight-model.lock
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' "$lock")
sum=$(sed -n 's/^checksum = "\(.*\)"/\1/p' "$lock")
[ -n "$ver" ] && [ -n "$sum" ] || { echo "No version or checksum in $lock."; exit 1; }
grep -q "^version = \"$ver\"" vendor/twilight-model/Cargo.toml || { echo "vendor/twilight-model is not version $ver."; exit 1; }
# A twilight bump that leaves vendor/ behind makes cargo drop the patch with only a warning.
! grep -q '^\[\[patch.unused\]\]' Cargo.lock || { echo "vendor/twilight-model is not used (patch.unused in Cargo.lock)."; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -sSfL -o "$tmp/c.crate" "https://static.crates.io/crates/twilight-model/twilight-model-$ver.crate"
echo "$sum  $tmp/c.crate" | sha256sum -c --quiet
tar xzf "$tmp/c.crate" -C "$tmp"
up="$tmp/twilight-model-$ver"
# Files cargo adds to a published crate; the vendored copy leaves them out.
rm -f "$up/.cargo_vcs_info.json" "$up/Cargo.lock" "$up/Cargo.toml.orig"
patch -s -p1 -d "$up" < vendor/twilight-model.patch
diff -r "$up" vendor/twilight-model || { echo "vendor/twilight-model differs from twilight-model $ver plus vendor/twilight-model.patch."; exit 1; }
echo "vendor/twilight-model is twilight-model $ver plus the recorded patch."
