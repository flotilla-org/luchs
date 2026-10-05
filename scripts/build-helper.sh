#!/bin/sh
set -eu
if [ "$(uname -s)" != Darwin ]; then
    echo 'The WKWebView helper requires macOS.' >&2
    exit 1
fi
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
destination=${1:-"$repo/target/debug"}
mkdir -p "$destination"
cc -Wall -Wextra -Werror -c "$repo/native/transport/SocketRights.c" -o "$destination/SocketRights.o"
swiftc -import-objc-header "$repo/native/transport/SocketRights.h" "$destination/SocketRights.o" -O -parse-as-library -framework AppKit -framework WebKit \
    "$repo/native/macos/CaptureInputWindow.swift" \
    "$repo/native/macos/SnapshotRecovery.swift" \
    "$repo/native/macos/PageAffordances.swift" \
    "$repo/native/macos/LuchsWebviewCapture.swift" \
    -o "$destination/luchs-webview-capture"

# The existing macOS CI job invokes this script. Validate the shared recovery
# policy alongside the helper without requiring a new workflow step.
if [ "${LUCHS_SKIP_NATIVE_TESTS:-0}" != 1 ]; then
    "$repo/scripts/test-helper.sh"
fi
