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
