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


Both live tests passed once more after replacing raw pipe setup/readiness calls
with rustix wrappers and preserving command-write diagnostics. The final Rust
pipe path therefore has live ping/reload/watch and clean-EOF evidence as well as
the fake-helper regression tests.

## Socketpair, BGRA and idle capture (2026-10-04)

Validated on the same macOS 26.6 arm64 desktop (25G72), Apple Swift 6.4
(swiftlang-6.4.0.34.1), Rust 1.98.0, and SDL 2.32.70. The producer toolkit and
reference viewer use Jackstay `9e6f145e5f9f2d1ba5732b0672a5ba6fb7c4b9ad`
(PR #77). That toolkit revision returns consumed pixel storage and lets luchs
retain logical input geometry across scaled CPU allocation replacements.

```sh
scripts/build-helper.sh
cargo test --locked --test live_macos -- --ignored --nocapture
cargo test --locked --test capture_policy --test helper_protocol
```

All four native tests passed: ping/reload/watch over the inherited socketpair,
clean socket EOF with a full-range u64 command ID, hidden/unchanged capture and
2x scale, and odd fractional pixel sizes. A 17x11 logical viewport produced
17x11, 26x17 and 34x22 frames at scale 1, 1.5 and 2, with native BGRA red pixels.
The last case caught AppKit rounding in `NSImage.cgImage`; drawing WebKit's
original image representation directly into the reused bitmap fixes it.

The fake-helper CLI test uses the real producer and presentation callback. It
checks unchanged publication sequences, two consecutive idle intervals of at
least 450 ms, wake within 200 ms after reload or a page signal, no capture
requests for 600 ms while hidden, immediate capture on re-show, a 2x pixel
allocation, and unchanged logical input dimensions. Frame decoder and toolkit
lifecycle tests check that recycled vectors retain their allocation pointers.

### SDL viewer and native editing

```sh
cargo build --workspace --locked --features backend-macos # in Jackstay
cmake -S tools/capture-viewer-sdl -B build/viewer
cmake --build build/viewer
# in luchs:
target/debug/luchs --stats --size=800x600 testdata/static.html
# pass its printed endpoint:
capture-viewer-sdl --source-socket "$source_path" --observe \
  --affordances optional --log-affordances
```

The production helper kept its transparent window ordered in. The live Retina
SDL viewer logged `source frame=800x600` followed by `source frame=1600x1200` as
its scale hint arrived. Desktop inspection showed the fixture title, click/key
readout and bar with sharp text. That static run completed 199 snapshots and
published just two frames (initial and 2x replacement); all 197 subsequent
snapshots were unchanged skips. Mean snapshot time was 3.989 ms and mean publish
time was 35.924 ms, including those two initial allocations. There were no later
publications after the idle threshold.

For native typing, a scratch build of the same Swift source made only its window
visible and mouse accepting, used a titled window and regular activation policy.
No capture, caret, transport or input script was changed. This made the helper's
own WebKit input field accessible to desktop automation without adding the later
Jackstay input slice. The CLI used `--helper` to select that scratch binary and
published `testdata/interactive.html` to the same SDL viewer. Typing `Frame skip`
and pressing Left updated the readout; subsequent Left presses moved the yellow
synthetic caret through the captured text. The viewer screenshot showed both the
typed value and caret between characters after its own window took focus. This
checks event-driven caret updates and page wake signals. It does not claim input
admission through the observation-only Jackstay channel.

An intermediate viewer launch failed with missing symbol
`ft_acquired_frame_macos_resources`: a local default-feature toolkit test build
had replaced `libjackstay.dylib` without `backend-macos`. Rebuilding with that
feature restored the viewer. The final live viewer inspection used the matching
native library and exited through source shutdown. Keep that feature enabled if
rebuilding Jackstay between live viewer runs.

### Timing measurements

The baseline was luchs `edf8a23dfb4500c8ed82809c25696c27832f41bf`'s
pre-change Swift helper, instrumented in a scratch file
around `takeSnapshot` and its complete emission function. It ran 90 snapshots at
30 fps over stdin/stdout; every snapshot was published. The static measurement
page was a dark background and one heading with no timers or CSS animations.
The updated CLI captured the same page for ten seconds at default `--fps=30`,
then exited on SIGTERM and printed `--stats`. The second size is a Retina-size
1600x1200 image; the separate SDL run above proves a scale hint reaches that
pixel size from an 800x600 logical viewport.

| Path | Pixels | Snapshots | Published | Unchanged skips | Mean snapshot ms | Mean publish ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Before, 90 samples | 800x600 | 90 | 90 | 0 | 2.279 | 10.317 |
| After, ten seconds | 800x600 | 33 | 1 | 32 | 2.422 | 27.461 |
| Before, 90 samples | 1600x1200 | 90 | 90 | 0 | 3.888 | 17.037 |
| After, ten seconds | 1600x1200 | 34 | 1 | 33 | 3.311 | 39.205 |

Snapshot time is WebKit API latency. Publish time includes drawing and transport;
the new path also hashes pixels. The after publish means each contain only one
cold-start publication, so they do not establish steady-state publish throughput.
These runs establish the removal of repeated static publications and the lower
idle snapshot rate. Direct arena writing and its copy-count measurement remain
#13's work. Small command/ack JSON and WebKit's own snapshot objects still
allocate; full-frame bitmap and receive buffers are reused after warm-up.

### Review regressions

The review follow-up replaces the 5 ms Rust receive poll with a deadline wait
interrupted by acknowledgement, state and presentation notifications. A fake
helper test verifies a 500 ms quiet wait, then prompt host, ack and page wakes.
The production watch/signal ceiling is 250 ms. Another subprocess test starts
with descriptors 0, 1 and 2 closed and verifies the inherited socket still
carries frames and acknowledgements. Visibility also applies when a requested
scale exceeds the capture limit.

The live macOS suite includes an HTTP server that deliberately leaves loading
unfinished for two seconds. Capture acknowledges without taking a snapshot;
ping, reload and presentation still complete within one second. Closing that
server without a response exercises the existing initial navigation failure
handlers and confirms that the helper exits with a diagnostic. Static pages now
stop the animation activity probe instead of running `getAnimations()` on every
animation frame. Snapshot completion also checks the window backing scale and
navigation state, discarding an invalidated snapshot and requesting a retry.

The final recovery policy allows two retries for consecutive invalid snapshots,
including WebKit errors or mismatched dimensions. The third reports the reason
to the CLI. `scripts/test-helper.sh` verifies that native policy, including budget
reset on success. The fake renderer discards two captures after an idle page
wake and verifies that no-report acknowledgements schedule the successful retry
with each next request within 250 ms instead of the 500 ms idle interval; another
CLI test verifies the exhausted-retry diagnostic.

Expected navigation or display-scale transitions discard an in-flight snapshot
without charging the failure budget. The native policy test covers that
distinction as well as genuine repeated failures. The helper build runs this
test by default for existing CI; local builders can set
`LUCHS_SKIP_NATIVE_TESTS=1` and run `scripts/test-helper.sh` separately.

## Native input executor (2026-10-04)

Validated on macOS 26.6 arm64 with Apple Swift 6.4, Rust 1.98.0 and
SDL 2.32.70. Both the Rust producer and the SDL reference viewer use Jackstay
`9e6f145e5f9f2d1ba5732b0672a5ba6fb7c4b9ad` (ABI 0.12). The viewer was built
from a scratch checkout of that exact revision; the workspace's newer ABI 0.13
viewer cannot attach to this producer. No dependency pin or viewer source changed.
The production helper was used with its transparent, mouse-ignoring window.

```sh
scripts/build-helper.sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check
cargo test --locked --test live_macos -- --ignored --nocapture
LUCHS_INPUT_TRACE=1 LUCHS_CONSOLE_LOG=/tmp/luchs-input-native.log \
  target/debug/luchs --endpoint=luchs-input-acceptance-2 --size=800x600 \
  testdata/interactive.html
capture-viewer-sdl --source-endpoint luchs-input-acceptance-2 \
  --typing cooperative --affordances optional --log-affordances
```

The ignored native suite passed all six tests. Its added input test receives
trusted DOM events from native responder calls, inserts `é🙂` through the text
input client, pairs a cooperative `z` press with its text commit, types physical
`KeyX` using the current layout, and checks native Cmd+A/C/V by duplicating
`é🙂zx`. Cleanup releases a held physical `KeyQ` and secondary button, while an
undelivered cooperative `w` press produces no keyDown. Four 0.25-point scrolls
accumulate into one native point; the CGEvent trace retains `fixed=-0.25`.
The test then scrolls a 2,000-point content region with an 80.5-point Pixel event,
a half-Line event (20 native points), and a quarter-Page event (150 native points).

The fake-helper suite covers ack success, unsupported rejection, failed and
missing ack uncertainty, fractional pointer coordinates, all three modes,
Line/Page conversion, scroll accumulation, shared command-port IDs, and the
library's cleanup barrier. A geometry reset sends pointer-only cleanup while a
key remains releasable by its original press identity. Replacement stays busy
until cleanup ack and executor completion; failed cleanup quarantines admission.

### SDL observations

The unmodified reference binary was copied into a scratch macOS application
bundle so desktop automation could select its window. No renderer or event path
was altered. The viewer received 1600x1200 pixels from the 800x600 logical source
on the Retina display.

| Mode / operation | Observed result in `testdata/interactive.html` |
| --- | --- |
| Cooperative click and typing | `clicks 1`; text field contains `Coop` |
| Cmd+A, Cmd+C, Right, Cmd+V | Field and readout contain `CoopCoop` |
| Text mode, typing ` text` | Field and readout contain `CoopCoop text`; helper trace has no key event for this commit |
| Physical mode, typing ` physical` | Field/readout show insertion at the clicked caret position; trace contains native key downs/ups from the current layout, with SDL text commits disabled |
| Automated precise scroll over the scroll region | Helper trace: `dx=-0.0 dy=847.9998779296875 fixed=-847.9998779296875 native=0.0,-847.0 precise=true` |

The shortcut check initially failed because the standalone WebKit host lacked
AppKit's Edit-menu key-equivalent dispatch. The helper now passes native events
through a standard Edit menu targeted at the bound view. The duplicate-text
assertion and SDL shortcut sequence above were rerun after that fix.
An earlier scroll check found Quartz's line-field setter overwriting point fields
and amplifying displacement by eight. Setting line, fixed-point, then point
fields produced the stated native values.

### Manual device follow-up

The governor amended [#2](https://github.com/flotilla-org/luchs/issues/2) on
2026-10-04: synthetic precise and Line scroll evidence satisfies its acceptance
criterion. Physical notched-wheel and trackpad checks are a manual operator task
in [#17](https://github.com/flotilla-org/luchs/issues/17), outside this change's
completion criteria. The automated Pixel path and Line/Page conversion evidence
above remain the recorded acceptance for #2. The earlier operator question is
withdrawn; no physical-device action is required to complete this PR.

### Review regressions

Scroll carry now commits only after an executed helper ack. Fake helpers check
that failed sends, unsupported/failed acks and timeouts do not consume the
previous fractional carry, and that values outside the 16.16 range are rejected
without sending a command. A helper that exits with code 23 during a key press
settles input as uncertain and leaves cleanup quarantined. The real CLI preserves
that renderer error and removes its endpoint, rather than masking the crash with
an input-cleanup diagnostic.

The producer now owns its serialized executor directly. A regression withholds
an input ack until the test reattaches the same helper, proving that attachment
is not blocked by the ack wait. Swift allocates the Edit menu only for Command
key equivalents; ordinary key presses take the native responder path directly.
The protocol documents `key_code=65535` for literal logical Unicode without a
physical binding and the retained holds after partial cleanup failure.

The review follow-up passed all 48 ordinary Rust tests, the locked workspace
build, clippy with warnings denied, formatting, Swift compilation and native
recovery checks. All six live helper tests passed with
`cargo test --locked --test live_macos -- --ignored --nocapture --test-threads=1`.
An earlier parallel run hit `EINVAL` while the existing slow-load HTTP fixture
set its accepted socket's timeout; the sequential rerun passed that fixture and
the other five tests. The native input test now verifies both Line and Page
conversion in real WebKit, in addition to the fractional Pixel sequence.

## Page affordances (2026-10-04)

Validated Luchs `482abfa4aebfc78ba1c40bcc507eddfc15b0f437` on the same
macOS 26.6 arm64 desktop, Apple Swift 6.4, Rust 1.98.0 and SDL 2.32.70.
Both Rust dependency pins and the reference viewer use Jackstay
`e5ad1a61e7317e0c6287216330b6104c5d415e96` (ABI 0.13). The previous 0.12 pin
could not attach to this viewer's six-object CPU setup; updating both pins
resolved that mismatch.

```sh
# Jackstay
cargo build --locked -p jackstay --features backend-macos
cmake -S tools/capture-viewer-sdl -B build/viewer
cmake --build build/viewer
# Luchs
scripts/build-helper.sh
cargo test --workspace --locked
cargo test --locked --test live_macos --test live_macos_affordances -- --ignored --nocapture
target/debug/luchs --endpoint=luchs-affordances-live --size=800x600 testdata/affordances.html
# Use the endpoint printed by Luchs:
capture-viewer-sdl --source-socket "$source_path" --observe \
  --affordances required --log-affordances
```

The 39 ordinary Rust tests passed, including the required-affordances fake-host
integration, all four initial domains and changed domains, helper replacement
withdrawals, URL accept/reject cases, and navigation/scroll forwarding.
The six ignored native tests also passed. Native Swift tests cover both snapshot
recovery and the independent engine URL policy, including symlink escape.
Clippy with denied warnings and the Rust 1.98 formatter passed.

The new native test uses the production CLI, helper and a required-affordances
host. It observed readiness after the first frame, a timed title update, and a
DOM height change from 2221 to 2421 CSS px. The document viewport was 623x463,
including WebKit's native scrollbar space. A small step reached y=40, a large
step reached y=456 (WebKit rounds the requested 416.7 px delta), and an oversized
position clamped to y=1958. Horizontal set-position reached x=200. Local-file
load, back, forward and reload completed. Back restored the history-cached title
and previous scroll position; a forced `pageshow` publication prevents the
helper's navigation reset from hiding that restored state. A javascript load
left the channel open and wrote a rejection to the console log.

### SDL navigation and scroll

The production transparent helper published the fixture to the real Retina SDL
viewer. Desktop inspection showed the heading, native content, toolbar and
scrollbar overlays; scale hints changed the source from 800x600 to 1600x1200.
Typing the second fixture's file URL into the viewer changed its title to
`Second affordance page`. Back restored `Luchs affordances`, forward restored
the second page, and reload produced another console-log navigation completion.
A vertical track click changed the published position from y=0 to y=524 and
moved the overlay thumb with the content.

A visible scratch helper was used for native page pointer and scroll interaction.
It changed only the activation policy and window presentation: a titled,
mouse-accepting, opaque window at normal level. WebKit configuration, capture,
protocol, state scripts and native page behavior were the production sources.
An app bundle around the unchanged SDL binary made it selectable by desktop
automation. This grants no Jackstay input authority.

A desktop thumb gesture subsequently published y=1136. The desktop automation
pipe closed during that gesture, so both-axis drag confirmation also used a
scratch copy of the viewer's SDL self-test driver. It computed tracks from the
real frame rectangle, including the navigation strip, then queued hover, down,
held motion and up through the normal SDL event routing. The live helper and
all producer/consumer transport code were unchanged. It reported:

```text
LIVE_DRAG_QUEUED axis=y from=276,51 to=276,144 target=1136.67
LIVE_DRAG_QUEUED axis=x from=107,204 to=181,204 target=431.667
LIVE_DRAG_PASS y=1136 target_y=1136.67 x=431 target_x=431.667
acquired_frames=200
```

The differences are WebKit's integer scroll positions. Native page scrolling
also produced successive scroll snapshots and the SDL overlay followed them.
Wheel input through an observation-only Jackstay connection remains outside this
slice; it belongs to input admission in #2.

### Cursor and title

The visible helper's text field, link and empty region produced cursor tags
10 (`text`), 5 (`pointer`) and 1 (`default`) in the SDL log. The script now tracks
mousedown as well as mousemove, and the native window accepts mouse-moved events,
so a click establishes the last pointer position even without a preceding move.
Clicking the fixture's title button changed both the window snapshot and SDL's
visible window title to `Title changed by the page`. Cursor checks use native
page interaction because the observation-only host cannot deliver pointer input.

Both source runs stopped on SIGTERM, and their viewers logged completed
affordances cleanup. Flotilla retains the live drag and cursor/title logs as
raw-test-output artifacts; the scratch driver and visible helper are outside
the repository.

### Review follow-up

HTTP and HTTPS navigation may target any host even when the startup page is
local, as required by issue #3. The directory boundary applies only to file
URLs; no optional remote-host allowlist was added. Swift readiness is advisory
until Rust confirms producer publication. The command-size allowance names the
26-byte maximum JSON command-ID field; the four-byte record prefix is outside
the JSON limit.

Review regressions add a parentless-startup-path error, Swift localhost-file
acceptance and localhost/directory-symlink escape rejection, and replacement
state arriving before the producer polls withdrawals. The latter checks that
all four withdrawals precede fresh snapshots and queued old commands are cleared.

The follow-up passed all 41 ordinary Rust tests, Clippy, formatting, helper
compilation and Swift policy tests. The production live page-affordances test
passed again after the review changes.
