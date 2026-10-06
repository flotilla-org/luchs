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
jackstay_manifest=$("$repo/scripts/pinned-jackstay.sh" manifest)
jackstay_include=$(dirname "$jackstay_manifest")/include
"$repo/scripts/pinned-jackstay.sh" dylib "$temporary"
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
