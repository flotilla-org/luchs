#!/bin/sh
# Resolve the Jackstay revision Cargo has locked for luchs and build its dylib.
# The helper's headers and dylib must come from the same revision as the Rust core.
#
# Usage: pinned-jackstay.sh manifest         print the locked Jackstay Cargo.toml
#        pinned-jackstay.sh dylib DIRECTORY  build it and copy libjackstay.dylib there
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
# POSIX sh has no pipefail: keep cargo's exit status by writing to a file.
cargo metadata --manifest-path "$repo/Cargo.toml" --locked --format-version 1 > "$temporary/metadata.json"
manifest=$(python3 - "$temporary/metadata.json" <<'PYTHON'
import json, sys
with open(sys.argv[1]) as file:
    packages = json.load(file)["packages"]
core, = [p for p in packages if p["name"] == "jackstay"]
producer, = [p for p in packages if p["name"] == "jackstay-producer"]
if core["source"] != producer["source"] or not core["source"].startswith("git+"):
    sys.exit("Jackstay and its producer toolkit must use the same pinned git revision")
print(core["manifest_path"])
PYTHON
)
case "${1:-}" in
manifest)
    echo "$manifest"
    ;;
dylib)
    destination=${2:?usage: pinned-jackstay.sh dylib DIRECTORY}
    # Always optimize the writer dylib. This separate target directory is shared
    # by the helper build and the native tests, so CI compiles Jackstay once.
    if ! cargo build --manifest-path "$manifest" --locked --release --lib -p jackstay \
        --target-dir "$repo/target/jackstay-helper"; then
        echo "Could not build pinned Jackstay with its committed lockfile: $manifest" >&2
        exit 1
    fi
    cp "$repo/target/jackstay-helper/release/libjackstay.dylib" "$destination/"
    ;;
*)
    echo 'usage: pinned-jackstay.sh manifest | dylib DIRECTORY' >&2
    exit 2
    ;;
esac
