#!/bin/sh
# Print the manifest path of the Jackstay revision Cargo has locked for luchs.
# The helper's headers and dylib must come from the same revision as the Rust core.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo metadata --manifest-path "$repo/Cargo.toml" --locked --format-version 1 | python3 -c '
import json, sys
packages = json.load(sys.stdin)["packages"]
core, = [p for p in packages if p["name"] == "jackstay"]
producer, = [p for p in packages if p["name"] == "jackstay-producer"]
if core["source"] != producer["source"] or not core["source"].startswith("git+"):
    sys.exit("Jackstay and its producer toolkit must use the same pinned git revision")
print(core["manifest_path"])
'
