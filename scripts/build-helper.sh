#!/bin/sh
set -eu
if [ "$(uname -s)" != Darwin ]; then
    echo 'The WKWebView helper requires macOS.' >&2
    exit 1
fi
for dependency in python3 otool; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "The WKWebView helper build requires $dependency." >&2
        exit 1
    fi
done
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
destination=${1:-"$repo/target/debug"}
mkdir -p "$destination"
# Resolve Cargo's locked dependency, rather than an independently selected
# Jackstay checkout. Its headers and dylib must match the Rust core's revision.
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
cargo metadata --manifest-path "$repo/Cargo.toml" --locked --format-version 1 > "$temporary/metadata.json"
jackstay_manifest=$(python3 - "$temporary/metadata.json" <<'PYTHON'
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
jackstay_include=$(dirname "$jackstay_manifest")/include
# Always optimize the writer dylib, including for a debug helper destination.
# This separate cache adds a cold Jackstay compile on the first helper/CI build.
if ! cargo build --manifest-path "$jackstay_manifest" --locked --release --lib -p jackstay \
    --target-dir "$repo/target/jackstay-helper"; then
    echo "Could not build pinned Jackstay with its committed lockfile: $jackstay_manifest" >&2
    exit 1
fi
cp "$repo/target/jackstay-helper/release/libjackstay.dylib" "$temporary/"
if ! otool -D "$temporary/libjackstay.dylib" | grep -Fx '@rpath/libjackstay.dylib' >/dev/null; then
    echo 'Jackstay dylib must have install name @rpath/libjackstay.dylib' >&2
    exit 1
fi
cc -Wall -Wextra -Werror -c "$repo/native/transport/SocketRights.c" -o "$temporary/SocketRights.o"
swiftc -import-objc-header "$repo/native/transport/SocketRights.h" "$temporary/SocketRights.o" -O -parse-as-library -framework AppKit -framework WebKit \
    -I "$jackstay_include" -L "$temporary" -ljackstay \
    -Xlinker -rpath -Xlinker @executable_path \
    "$repo/native/macos/CaptureInputWindow.swift" \
    "$repo/native/macos/SnapshotRecovery.swift" \
    "$repo/native/macos/PageAffordances.swift" \
    "$repo/native/macos/WriterImport.swift" \
    "$repo/native/macos/LuchsWebviewCapture.swift" \
    -o "$temporary/luchs-webview-capture"
if ! otool -L "$temporary/luchs-webview-capture" | grep -F '@rpath/libjackstay.dylib (' >/dev/null; then
    echo 'Helper must link @rpath/libjackstay.dylib' >&2
    exit 1
fi
if ! otool -l "$temporary/luchs-webview-capture" | grep -F 'path @executable_path (offset ' >/dev/null; then
    echo 'Helper must have an @executable_path rpath' >&2
    exit 1
fi
cp "$temporary/libjackstay.dylib" "$temporary/luchs-webview-capture" "$destination/"

# The existing macOS CI job invokes this script. Validate the shared recovery
# policy alongside the helper without requiring a new workflow step.
if [ "${LUCHS_SKIP_NATIVE_TESTS:-0}" != 1 ]; then
    "$repo/scripts/test-helper.sh"
    swiftc -O -parse-as-library -I "$jackstay_include" -L "$temporary" -ljackstay \
        -Xlinker -rpath -Xlinker @executable_path \
        "$repo/native/macos/WriterImport.swift" \
        "$repo/native/tests/WriterImportTests.swift" \
        -o "$temporary/writer-import-tests"
    "$temporary/writer-import-tests"
fi
