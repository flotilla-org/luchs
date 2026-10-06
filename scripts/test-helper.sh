#!/bin/sh
set -eu
if [ "$(uname -s)" != Darwin ]; then
    echo 'The native helper tests require macOS.' >&2
    exit 1
fi
for dependency in cargo python3 swiftc; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "The native helper tests require $dependency." >&2
        exit 1
    fi
done
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
cc -Wall -Wextra -Werror -I "$repo/native/transport" \
    "$repo/native/transport/SocketRights.c" \
    "$repo/native/tests/SocketRightsTests.c" \
    -o "$temporary/socket-rights-tests"
"$temporary/socket-rights-tests"
swiftc -O -parse-as-library \
    "$repo/native/macos/SnapshotRecovery.swift" \
    "$repo/native/tests/SnapshotRecoveryTests.swift" \
    -o "$temporary/snapshot-recovery-tests"
"$temporary/snapshot-recovery-tests"
swiftc -O -parse-as-library \
    "$repo/native/macos/PageAffordances.swift" \
    "$repo/native/tests/PageAffordancesTests.swift" \
    -o "$temporary/page-affordances-tests"
"$temporary/page-affordances-tests"
# Headless writer-import tests link the pinned Jackstay dylib, built exactly as
# scripts/build-helper.sh builds it. Its install name is @rpath/libjackstay.dylib,
# so the test binary finds the copy beside it through the @executable_path rpath.
jackstay_manifest=$("$repo/scripts/pinned-jackstay.sh" manifest)
jackstay_include=$(dirname "$jackstay_manifest")/include
"$repo/scripts/pinned-jackstay.sh" dylib "$temporary"
swiftc -O -parse-as-library -I "$jackstay_include" -L "$temporary" -ljackstay \
    -Xlinker -rpath -Xlinker @executable_path \
    "$repo/native/macos/WriterImport.swift" \
    "$repo/native/tests/WriterImportTests.swift" \
    -o "$temporary/writer-import-tests"
"$temporary/writer-import-tests"
