#!/bin/sh
# Requires a logged-in macOS desktop; deliberately separate from headless tests.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
swiftc -O -parse-as-library -framework AppKit \
    "$repo/native/macos/CaptureInputWindow.swift" \
    "$repo/native/tests/CaptureInputWindowTests.swift" \
    -o "$temporary/capture-input-window-tests"
"$temporary/capture-input-window-tests"
