# Live macOS acceptance

Run on 2026-10-03 in the logged-in desktop session, with real `WKWebView`
snapshots at 800x600. Both the local interactive page and an HTTPS page reached
Jackstay's SDL reference viewer and `katzensteg jackstay-source`.

| Component | Revision / version |
| --- | --- |
| Luchs implementation | `0275a8c4eeac13bf7b315fa6bf07020e7e88574d` |
| Jackstay Rust dependency and SDL viewer | `33369151429d82a2b923ab2891e199b05aca075a` |
| Katzensteg | `c981b7d482d57ddd0f16fae9b1c363b741a9b3a6` |
| Katzensteg's separately pinned Jackstay C library | `94a697e6ace998700fa7342b7bc7f217c496b367` (ABI 0.8) |
| OS | macOS 26.6, build 25G72, arm64 |
| Swift | Apple Swift 6.4, swiftlang-6.4.0.34.1 |
| Rust | 1.98.0, 88d9e12ae |

The implementation commit was rewritten after validation to separate the CI
workflow for governor delivery. Its Rust sources, helper and fixtures are byte
identical to the tested commit `0078490a99d62a8bb3f97ff0ff56f5dbec446c06`.

The existing Katzensteg launcher had Jackstay disabled. A clean scratch clone of
the same revision was built with `zig build -Djackstay=true
-Djackstay-prefix=/Users/robert/.cache/katzensteg/jackstay`. The prepared prefix
matched its committed dependency pin. No Katzensteg source was changed.
The reference viewer was built from the listed Jackstay revision with
`backend-macos`, using SDL 2.32.70.

## Commands and results

Each source ran unbounded so consumers could disconnect without stopping it:

```sh
LUCHS_CONSOLE_LOG=/tmp/interactive-console.log \
  target/debug/luchs --endpoint=luchs-live-interactive --size=800x600 testdata/interactive.html
LUCHS_CONSOLE_LOG=/tmp/https-console.log \
  target/debug/luchs --endpoint=luchs-live-https --size=800x600 https://example.com
```

For each printed socket path, the consumers ran:

```sh
capture-viewer-sdl --source-socket "$source_path" --observe --frames 300
katzensteg jackstay-source "$source_path" --observe
```

| Page | SDL result | Katzensteg result |
| --- | --- | --- |
| `testdata/interactive.html` | Exit 0, `acquired_frames=300`; visible fixture title and control panels | Live process, cleat render generation 54, `images=1/1` |
| `https://example.com` | Exit 0, `acquired_frames=300`; visible multilingual domain notice | Live process, cleat render generation 156, `images=1/1` |

The SDL windows were inspected through the desktop screenshot tool. Katzensteg
ran in a recorded cleat terminal with a functional Ghostty VT. Its render-packet
summaries showed the uploaded and placed image; its logs reported supported
graphics, successful shared-memory probing and direct-TTY fullscreen presentation.
This terminal evidence does not claim a kitty screenshot: the computer-use tool
refused access to that app.

Both WebKit console logs contained navigation completion, `requestAnimationFrame
fired`, and `after 2s: visibility visible, hasFocus false, requestAnimationFrame
ran`. The helper kept running after the bounded SDL consumers exited, and a
second SDL viewer connected successfully. SIGTERM then stopped each Luchs
process cleanly, reaped its helper and removed its source endpoint.

An initial live run found a missing `CpuCopyComplete` descriptor value, which
Katzensteg rejects. The implementation revision above fixes that field and adds
a regression assertion; the results in the table come from rerunning that build.

The test suite independently covers resize, holding an old lease across a
replacement, consumer process death with a held lease, reload through a fake
helper, and SIGTERM cleanup. Input delivery is outside this slice.

## Bootstrap v2 migration (2026-10-04)

Jackstay is now pinned to `ed785976df1246d2d3ce3f0c41df91c02f38e20d`
(ABI 0.12). The evidence above predates this migration and covers bootstrap v1.
The v2 live check remains pending: Jackstay #59, the SDL reference viewer's
bootstrap v2 slice, is still open. After it lands, present
`testdata/interactive.html` using that viewer on a live macOS desktop and append
the tested revisions and results here. Automated tests exercise v2 media, optional
controls, replacement, process death and SIGTERM with a live consumer.


## Framed helper protocol (2026-10-04)

Validated on the logged-in macOS 26.6 arm64 desktop with Apple Swift 6.4
(swiftlang-6.4.0.34.1) and Rust 1.98.0. Jackstay remains pinned to
`ed785976df1246d2d3ce3f0c41df91c02f38e20d` (ABI 0.12).

```sh
scripts/build-helper.sh
cargo test --locked --test live_macos -- --ignored --nocapture
```

`native_ping_reload_and_watch` passed. It received real 32x32 WebKit frames,
confirmed `ping` and `reload` returned `Executed`, and confirmed the old
`mouse_down` command returned `Unsupported`. Rewriting a red page to blue and
reloading changed the first captured pixel from `[255, 0, 0, 255]` to
`[0, 0, 255, 255]`.

The test then launched the real CLI with `--watch --size=32x32 --fps=15`,
connected through bootstrap v2, and acquired both the original red pixels and
blue pixels after editing the file. This exercises file polling, framed stdin,
main-thread reload, ack demultiplexing, and publication through Jackstay. The
CLI stopped successfully on SIGTERM after six frames and removed its endpoint.
No SDL viewer was needed for this protocol acceptance check; the earlier v2
presenter check remains separate.


The review follow-up also ran `native_stdin_eof_exits_successfully`: a framed
ping with ID `18446744073709551615` round-tripped without losing bits, and closing
stdin exited the real Swift helper with status zero while a reader drained
stdout. The ping/reload/watch acceptance test passed again after the Rust
recovery changes. Fake-helper CLI tests separately cover retrying failed and
uncertain reloads without another file modification, and stopping on an
unsupported reload.
