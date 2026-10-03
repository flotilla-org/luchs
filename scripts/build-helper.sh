#!/bin/sh
set -eu
if [ "$(uname -s)" != Darwin ]; then
    echo 'The WKWebView helper requires macOS.' >&2
    exit 1
fi
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
destination=${1:-"$repo/target/debug"}
mkdir -p "$destination"
swiftc -O -parse-as-library -framework AppKit -framework WebKit \
    "$repo/native/macos/LuchsWebviewCapture.swift" \
    -o "$destination/luchs-webview-capture"
