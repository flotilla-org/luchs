#!/bin/sh
set -eu
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
# Headless writer-import tests link the pinned Jackstay dylib, built as
# scripts/build-helper.sh builds it (cached in the same target directory).
jackstay_manifest=$("$repo/scripts/pinned-jackstay.sh")
jackstay_include=$(dirname "$jackstay_manifest")/include
cargo build --manifest-path "$jackstay_manifest" --locked --release --lib -p jackstay \
    --target-dir "$repo/target/jackstay-helper"
cp "$repo/target/jackstay-helper/release/libjackstay.dylib" "$temporary/"
swiftc -O -parse-as-library -I "$jackstay_include" -L "$temporary" -ljackstay \
    -Xlinker -rpath -Xlinker @executable_path \
    "$repo/native/macos/WriterImport.swift" \
    "$repo/native/tests/WriterImportTests.swift" \
    -o "$temporary/writer-import-tests"
"$temporary/writer-import-tests"
