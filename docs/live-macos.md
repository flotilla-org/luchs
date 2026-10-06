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

This records PR #16 before it was combined with page affordances. The rebased
producer retains native input and uses ABI 0.13; the 0.12 measurements below
describe that earlier validation run.

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

This records the affordances branch before native input landed. The rebased
producer now delivers host pointer events through the native input executor;
the observation-only limits below describe the original evidence run.

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

## Combined input and affordances after rebase (2026-10-04)

Rebased onto `d6ee0dc`, which landed native input from PR #16. The producer keeps
its input executor, capabilities and cleanup path alongside complete page-state
snapshots and navigation/scroll verbs. Swift decodes both command families,
including the explicit CodingKeys for navigation and document scrolling. The
older validation sections above retain the revisions and limits of their runs.

The pinned CI commands passed locally: Rust 1.98 formatting, stable locked
workspace build and all 54 ordinary tests, Clippy with warnings denied, helper
compilation, and Swift recovery/URL policy tests. All eight ignored live macOS
tests passed sequentially. The added combined test uses one v2 connection with
required SourceText input and required affordances. It verifies a trusted native
click at x=20,y=20, cursor `text`, UTF-8 insertion `é🙂` reflected in window title,
document y=40 positioning, a file load to `Combined next`, and clean input close.

A host motion probe completed as executed but produced no DOM mousemove on this
machine. The existing direct native motion path is retained; investigation is
tracked in [#19](https://github.com/flotilla-org/luchs/issues/19). Native clicks do
produce pointer-driven cursor state, and the original visible-window cursor
checks remain recorded above. No experimental hover routing is included.

## Native host hover diagnosis (2026-10-05)

This section supersedes the unresolved hover finding and the "no experimental
hover routing" statement in the combined-input section above. Those paragraphs
record the earlier PR #18 validation.

Issue #19 reproduced on macOS 26.6 (25G72), arm64. A new ignored test starts
the production CLI and transparent helper, connects one v2 host with required
SourceText input and required affordances, waits for `window.ready`, then sends
Motion at logical (20,70) over a link at top=60. Its page listener only records
native events; it never dispatches JavaScript events.

```sh
scripts/build-helper.sh
cargo test --locked --test live_macos_affordances \
  native_host_motion_delivers_trusted_dom_hover -- --ignored --nocapture
```

Before the fix, two runs failed in 3.51 and 3.40 seconds: the host received
`Outcome::Executed`, but the console had no `mousemove 20,70 trusted=true`.
Removing the input field and all other page behavior preserved the failure.
Four hypotheses, ranked before probes, were responder routing, key/front-window
hover gating, event coordinates/window identity, and window mouse-event flags.
Temporary native instrumentation and helper variants stayed outside the checkout.

| Isolated probe | Result |
| --- | --- |
| Original `WKWebView.mouseMoved(with:)` | No DOM motion; window key=false, main=false |
| Use the original NSEvent without CGEvent round-trip | No DOM motion |
| `ignoresMouseEvents=false`, with original routing | No DOM motion |
| `NSWindow.sendEvent`, with original window status | No DOM motion |
| Forward to the `.mouseMoved` tracking-area owner | No DOM motion with key=false |
| Report local key status, with original routing | No DOM motion |
| Report local main status, with tracking-owner routing | No DOM motion |
| Report local key status, with tracking-owner routing | Trusted DOM motion and cursor `pointer` |

`acceptsMouseMovedEvents` was already true. Before and after CGEvent conversion,
the event's window number matched the capture window and its location was
(20,410), the correct AppKit position for logical (20,70) in a 480-point view.
The frontmost application stayed `work.flotilla.wheelhouse` during these probes.

There are two necessary changes. WKWebView inherits the responder's mouseMoved
implementation; WebKit receives native hover through an AppKit tracking-area
owner. Its embedded page also needs local key status for hover delivery on this
system. The routing diagnosis agrees with [WebKit bug 323489](https://bugs.webkit.org/show_bug.cgi?id=323489)
and its [native regression tests](https://github.com/WebKit/WebKit/blob/cc84d838c54ffb5a424cbea22c15f3fe1ac09cf1/Tools/TestWebKitAPI/Tests/WebKit/WebPage/WebPageMouseEventsTests.swift).
Those tests use a window subclass that reports key status.

`CaptureInputWindow` reports `isKeyWindow=true` to WebKit while refusing eligibility
for AppKit key/main status. The helper retains its prohibited activation policy
and orders the transparent, mouse-ignoring window without calling `makeKey`,
`makeMain`, or activation methods. This gives the page local focus (including
`document.hasFocus()`), so page focus behavior differs from the old inactive page.
It does not transfer desktop focus. The live window test asserts the distinction:

```sh
scripts/test-hover-window.sh
# Capture input window: local key status, no AppKit key/main window or activation,
# frontmost application preserved
```

Hover forwarding uses the owner exposed by the public AppKit tracking-area API
and its `mouseMoved:` selector. It does not name WebKit private classes or call
private WebKit selectors. If no capable owner exists, the helper returns
`unsupported` instead of acknowledging the inherited no-op. This still depends
on WebKit retaining a mouse-moved tracking area; the live regression detects
changes in that behavior on future macOS versions.

After the fix, the minimized test passes and records `mousemove 20,70 trusted=true
hover=true`, proving both trusted DOM delivery and CSS `:hover`. It observes
cursor `pointer`, moves off the link to `default`, clicks the input to `text`,
then hovers the link again and verifies a second trusted event and `pointer`.
Input cleanup and SIGTERM shutdown pass. A separate scratch monitor sampled the
frontmost process every 5 ms throughout this production test; it stayed unchanged.

All nine ignored live macOS tests passed sequentially, including the existing
combined click/text/title/scroll/navigation test. The locked workspace build,
54 ordinary Rust tests, Clippy with denied warnings, Rust 1.98 formatting,
Swift helper build, and native recovery/URL policy tests passed. The desktop
focus test is separate from the headless policy suite because it requires a
logged-in macOS session.

## Direct arena snapshots (2026-10-05, issue #13)

Validated in the logged-in macOS 26.6 (25G72), arm64 desktop with Rust 1.98.0
and Apple Swift 6.4. Jackstay remains pinned to
`e5ad1a61e7317e0c6287216330b6104c5d415e96`; no Jackstay change was needed.
The implementation is the commit adding this section, based on Luchs
`5e45a1f7884b05ce11f4062508008a342cb71056`.

The helper receives only the writable payload object with `SCM_RIGHTS`. Rust
reserves a slot, the helper draws the native WebKit representation into its
BGRA mapping, and an acknowledgement permits Rust to commit that slot.
Retired frame tag 1 now fails the protocol. There is no socket pixel fallback.

The explicit full-frame destination writes per snapshot fall from three in the
frame-path slice to one: previously the CGContext draw, the Rust socket receive
buffer fill and the toolkit's arena copy; now only the CGContext draw into the
slot. This count excludes framework-internal WebKit work and kernel transport
copies, and does not count hashing reads. `copies_per_frame=1` in stats records
that path invariant, rather than a hardware performance-counter measurement.

### Timing

Release Rust binaries and `swiftc -O` helpers ran at `--fps=30` with no consumer.
The static fixture used a dark background and one heading, matching the workload
described in the frame-path timing section above. Each static run lasted ten
seconds before SIGTERM. The animated fixture changed the heading in each rAF;
`--frames=90` stopped after 90 committed changed frames. The 1600x1200 output is
the same pixel count as the earlier Retina measurement.

| Path and workload | Pixels | Snapshots | Published | Skipped | Copies | Mean snapshot ms | Mean publish ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Frame-path slice, historical static run | 800x600 | 33 | 1 | 32 | 3 | 2.422 | 27.461 |
| Direct arena, static ten seconds | 800x600 | 27 | 1 | 26 | 1 | 1.018 | 2.240 |
| Frame-path slice, historical static run | 1600x1200 | 34 | 1 | 33 | 3 | 3.311 | 39.205 |
| Direct arena, static ten seconds | 1600x1200 | 26 | 1 | 25 | 1 | 3.569 | 6.748 |
| Direct arena, animated 90 publications | 1600x1200 | 90 | 90 | 0 | 1 | 2.587 | 5.213 |

The timing samples were collected at `5a31d9bac32a6faf2b6f5acdbfb969c399ad3862`,
before the review follow-ups below. Those changes add no pixel copy and retain
the same helper timing boundary.

The two static publish means each contain a single cold publication and come
from separate runs. They do not establish a steady-state speedup. The new
animated row measures repeated changed publications; there is no matching
historical animated row. Publish time now covers drawing and hashing through
completion of the slot write. The earlier path also included draining all pixels
to the socket. The small JSON acknowledgement and Rust commit are outside the
helper's timing window.

Reproduce the fixtures and commands with:

```sh
cargo build --release --locked
scripts/build-helper.sh target/release
measurement=$(mktemp -d)
cat > "$measurement/static.html" <<'HTML'
<!doctype html><style>body{margin:0;background:#18212b;color:white}</style><h1>Frame path measurement</h1>
HTML
cat > "$measurement/animated.html" <<'HTML'
<!doctype html><style>body{margin:0;background:#18212b;color:white}</style><h1 id="heading">Frame path measurement</h1><script>let n=0;function tick(){heading.textContent="Frame path measurement "+(++n);requestAnimationFrame(tick)}requestAnimationFrame(tick)</script>
HTML
# Run each size separately, then send SIGTERM after ten seconds.
target/release/luchs --size=800x600 --fps=30 --stats "$measurement/static.html"
target/release/luchs --size=1600x1200 --fps=30 --stats "$measurement/static.html"
target/release/luchs --size=1600x1200 --fps=30 --stats --frames=90 "$measurement/animated.html"
```

All runs exited successfully. Flotilla retains the raw stderr stats as an artifact.

### Validation

The 61 ordinary Rust tests passed, as did workspace build, Clippy with warnings
denied and Rust 1.98 formatting. Fake helpers map the actual exported fd and
write known binary patterns; real consumers observe byte-exact frames across
resize while an old lease stays valid. Unchanged, hidden, timeout, helper death
(including death after ack), invalid ID/slot/generation/header and capacity-paused
resize cases verify abandonment or retry. Resize unmaps the helper's old writer
before requesting the replacement, keeping it from pinning a paused allocation.

The production Swift helper compiled and its native recovery/URL tests passed.
All nine ignored live macOS tests passed sequentially: native ping/reload/watch,
socket EOF, hidden/unchanged draws and scale, odd fractional pixel dimensions,
slow-load command service, native input/cleanup, page affordances, and combined
input with affordances, plus the native trusted-hover regression from PR #20.
The separate desktop-focus test passed after the rebase. The live CLI consumers acquired red and blue pixels
through bootstrap v2. SIGTERM completes an active draw before ordered native input
cleanup and removes the endpoint.

Review follow-up adds an explicit unmap before toolkit shutdown, with a fake
helper confirming cleanup and ping still execute afterward. The native hidden
capture test now re-shows identical pixels at the same scale and allocation,
verifying the existing helper fingerprint reset after an abandoned draw. Cached
contexts reject dimension/stride changes. Rust event waits distinguish wake,
timeout and closure; draw reservations remain guarded even if command send fails.


## Presentation resize and focus (2026-10-05, issue #7)

Validated on macOS 26.6 (25G72), arm64, Apple Swift 6.4, Rust 1.98.0 and
SDL 2.32.70. Luchs is based on `eebd2c7`; the implementation is
[Luchs PR #23](https://github.com/flotilla-org/luchs/pull/23). The producer toolkit and SDL viewer use Jackstay
`c3b88ec3badc278d986ea97d3e6e6e8801953193` (direct arena input geometry callback).
The reviewed Luchs pin was `6516094b8f4335b54d06bba8586a64d67ab0d9c6`, which
consolidates that callback with the copied-frame geometry path and adds cleanup
and invalid-geometry tests. The native presentation regression passed again
with this reviewed pin. Jackstay PR #86 was then squash-merged as
`276900db59b9c651b1f5a83427a6f498265770fd`; both Luchs dependencies now pin
that merged commit. Its source tree is identical to the reviewed pin.

```sh
scripts/build-helper.sh
cargo test --locked --test presentation
cargo test --locked --test live_macos_presentation -- --ignored --nocapture
# Build the matching Jackstay library with backend-macos, then its SDL viewer.
target/debug/luchs --endpoint=luchs-resize-focus-live --stats --size=800x600 testdata/presentation.html
capture-viewer-sdl --source-endpoint luchs-resize-focus-live --typing cooperative --affordances required --log-affordances
```

The live SDL window ran on the Retina desktop. Its initial preferred viewport
was 320x180 at scale 2. Dragging its corner produced 620x416 logical units with
one card column, then 820x466 with two columns. Shrinking to 520x366 stacked the
cards again. Desktop screenshots showed those sizes and column counts rendered
inside the viewer, with the synthetic caret in the focused input. WebKit's
console recorded the same reflows. Closing the viewer withdrew presentation;
the page returned to 800x600, two columns, unfocused, with the caret hidden.
The viewer executable was wrapped in a temporary app bundle for desktop
automation; its binary and rendering/input code were unchanged.

The native regression used the production helper and real bootstrap host. It
changed 800x600 to 500x400 at scale 1.5, acquired a 750x600 BGRA frame, and saw
input geometry revision 2 with logical width 500. The page reported one column
and `focused=true caret=block`; focus false reported `caret=none` without
changing geometry. Withdrawal acquired an 800x600 frame and advanced revision
to 3. SIGTERM exited successfully after releasing the consumer.

The fake-helper test checks acknowledged resize/focus commands, scaled frame
sizes, unchanged geometry on scale-only updates, latest-wins bursts, and all
withdrawal defaults. Focus-only changes preserve controller admission and
emit no cleanup. Size tests cover fractional rounding, the exact cap, very large
square and narrow requests, and small scales.


Review follow-up replaces the timing-dependent burst assertion with a deterministic
clock test and adds invalid-scale and non-finite-size coverage. A fake helper
fails the first focus acknowledgement after a successful resize; capture keeps
publishing at the new size and the next hint retries focus without another resize.
The locked Rust suite, build, Clippy and formatting passed, as did the native
resize/focus regression with the revised scheduler. A broader native-affordance
rerun reported `visibility hidden` and suspended rAF in the current desktop
session; those same fixtures passed during the earlier live validation above.

The second review added `native_frame_cap_resize_acks_fit_command_timeout`.
Using the production helper and one-second command timeout, a 2048x2048 logical
resize at scale 2 acknowledged in 4.936 ms; 4096x4096 at scale 1 took 1.883 ms;
restoring 800x600 took 1.376 ms. Both large requests reach the 64 MiB pixel cap.
These timings cover viewport command execution, not snapshot completion, which
has its own draw deadline. Both native presentation tests passed sequentially.

Linux CI exposed a failed-draw acknowledgement racing with helper EOF. Rust now
validates an available acknowledgement before reporting writer death, retaining
the snapshot failure detail while refusing to commit pixels from an exited
writer. A deterministic dead-writer regression and the existing CLI exhausted
snapshot retry test passed with this fix.

## Jackstay C writer API (2026-10-06, issue #22)

Validated on macOS 26.6 (25G72), arm64, Apple Swift 6.4, Rust 1.98.0 and
SDL 2.32.70. This change is based on Luchs `08c379e`; both Rust dependencies and
the helper's headers/dylib pin Jackstay `5fcc8dc285dcb059f9ceb9f976cfb3b8db8b15e2`.

The locked workspace build, 66 ordinary Rust tests, denied-warning Clippy and
Rust 1.98 formatting passed. Helper builds in debug and release destinations,
the native descriptor/recovery/URL tests, and the desktop-focus test passed.
All 12 ignored live macOS tests passed sequentially:

```sh
scripts/build-helper.sh
cargo test --locked --test live_macos --test live_macos_affordances \
  --test live_macos_presentation -- --ignored --nocapture --test-threads=1
scripts/test-hover-window.sh
```

The new regression draws into a cached slot, rejects a foreign scope, stale
generation and out-of-range index, and then re-exports after a viewport resize.
It rejects the old generation using dimensions valid for the replacement,
rejects stale release, releases the current writer, and imports/draws again.
The existing real-host test also verifies resize and withdrawal through the
production Rust scheduler. A grep for `mmap` or arithmetic involving
`slot_capacity` in the Swift helper returns no matches.

A helper and dylib copied into a fresh temporary directory loaded without DYLD
path overrides. Removing that adjacent dylib produced
`Library not loaded: @rpath/libjackstay.dylib`. Replacing it with a disposable C
stub reporting ABI 14 produced `Jackstay ABI mismatch: helper expects 13, library
reports 14` and exited with status 1 before WebKit startup. Build checks verify
the install name, helper dependency and `@executable_path` rpath with `otool`.

The matching SDL viewer displayed `testdata/presentation.html` from the release
CLI through bootstrap v2. Its visible frame showed the page heading and
`320x180 columns=1`. Closing the viewer and sending SIGTERM to Luchs completed
successfully, with `copies_per_frame=1`.

### Timing comparison

Used the same static and animated fixtures and commands as the issue #13
measurement above. Each static run lasted ten seconds; each animated run ended
after 90 changed publications. The baseline helper was compiled with `swiftc -O`
from this branch's base commit, with its original arena mapping. Both helpers
used this change's release Rust binary, isolating the helper change.

| Helper / workload | Pixels | Snapshots | Published | Skipped | Copies | Mean snapshot ms | Mean publish ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Baseline, static | 800x600 | 29 | 1 | 28 | 1 | 2.222 | 0.979 |
| C writer API, static | 800x600 | 27 | 1 | 26 | 1 | 1.979 | 0.981 |
| Baseline, static | 1600x1200 | 26 | 1 | 25 | 1 | 4.432 | 3.554 |
| C writer API, static | 1600x1200 | 27 | 1 | 26 | 1 | 4.889 | 3.158 |
| Baseline, animated run 1 | 1600x1200 | 90 | 90 | 0 | 1 | 2.552 | 3.921 |
| C writer API, animated run 1 | 1600x1200 | 90 | 90 | 0 | 1 | 2.484 | 4.265 |
| C writer API, animated run 2 | 1600x1200 | 90 | 90 | 0 | 1 | 2.919 | 4.943 |
| Baseline, animated run 2 | 1600x1200 | 90 | 90 | 0 | 1 | 1.794 | 4.664 |
| C writer API, animated run 3 | 1600x1200 | 90 | 90 | 0 | 1 | 1.879 | 4.417 |
| Baseline, animated run 3 | 1600x1200 | 90 | 90 | 0 | 1 | 1.578 | 4.333 |

All runs exited successfully. The one-copy frame path, static unchanged skips
and all 90 animated publications are preserved. The C API animated publish means
remain below the issue #13 measurement of 5.213 ms. Against the current baseline,
the three-run publish averages are 4.542 ms versus 4.306 ms; their ranges overlap.
These short desktop samples cannot establish a small timing regression or
speedup. Static publish means each contain only one cold publication. Raw stats
and native test output are retained as Flotilla artifacts.

Review follow-up extracts the production writer import into `WriterImport.swift`
and runs its tests from the existing macOS CI helper-build step, without WebKit
or a desktop session. A real C producer/export supplies the positive import.
Malformed JSON scope lengths (0, 15, 17) and C-rejected zero generation, length,
capacity, slot count and insufficient mapping length all release the received fd;
the export's original fd remains open. Successful writer destruction releases its
owned fd. The helper/native tests and all 12 live tests passed after extraction.
Build dependency mutations also verified that missing `python3` and `otool`
produce diagnostics naming the missing tool. README now explains that an
external `--helper PATH` must have its dylib in its own directory.

Second re-review adds symbolic names to the import API's error statuses while
retaining their numbers. The headless invalid-layout cases verify the actual
`FT_STATUS_ERROR` diagnostic and fd release. The macOS CI helper-build step
leaves `LUCHS_SKIP_NATIVE_TESTS` unset, so it executes these tests by default.

## End-to-end SDL acceptance (2026-10-06, issue #25)

Ran the [#25 checklist](https://github.com/flotilla-org/luchs/issues/25) from
[#8](https://github.com/flotilla-org/luchs/issues/8) in the logged-in macOS
26.6 (25G72), arm64 desktop. 10 of 11 items passed; item 3 failed its native automated
thumb-drag check. The failure is filed as
[Jackstay #87](https://github.com/flotilla-org/jackstay/issues/87). No runtime
source or dependency pin was changed. Physical wheel and trackpad testing remains
outside this slice in [#17](https://github.com/flotilla-org/luchs/issues/17).

| Component | Tested revision / version |
| --- | --- |
| Luchs current `main` | `dbd7aa086c9ee4c18c7fa7eaa9917b936948817d` |
| Jackstay current `main`, SDL viewer and both Luchs dependency pins | `5fcc8dc285dcb059f9ceb9f976cfb3b8db8b15e2` (ABI 0.13) |
| Rust | 1.98.0 (`88d9e12ae`) |
| Swift | Apple Swift 6.4 (`swiftlang-6.4.0.34.1`) |
| SDL | 2.32.70 |

Both checkouts matched freshly fetched `origin/main` before building. The viewer
executable was copied unchanged into `/tmp/Luchs25Viewer.app`, with
`NSHighResolutionCapable=true`, so desktop automation could select it. The
production helper retained its transparent, mouse-ignoring window and prohibited
activation policy. No visible-helper variant or modified viewer was used.

The evidence archive is Flotilla raw-test-output
`artifact/artifact-9067d61edc23c6f3deb50e49cb315f44ed19b5587e0d116b6fec9c798db61c97`
(SHA-256 `3a31ed7ffabcad8719c2a6d8a72c032967eed68743f06951ec2301d6d3dc685b`).
It contains the build recordings, source/viewer console logs, screenshots,
measurement commands/results, and the standalone C probes described below.
The raw archive is scheduled to expire on 2026-10-13. Retrieve it before that
date if screenshots or probe sources are needed; the checklist, commands, key
log excerpts and measurement tables remain in this repository after expiry.
The archive can be retrieved with `flotilla artifact get` and its identity above,
using `--output /tmp/luchs25-evidence.tar.gz`.
These are fresh results; earlier dated sections retain their original scope.

### Build and live commands

```sh
# Jackstay, at the main revision above:
cargo build --workspace --locked --features backend-macos
cmake -S tools/capture-viewer-sdl -B build/viewer
cmake --build build/viewer
ctest --test-dir build/viewer --output-on-failure

# Luchs, at the main revision above:
cargo build --release --locked
scripts/build-helper.sh target/release
install -d "$HOME/.local/bin"
install target/release/luchs target/release/luchs-webview-capture \
  target/release/libjackstay.dylib "$HOME/.local/bin/"

LUCHS_INPUT_TRACE=1 LUCHS_CONSOLE_LOG=/tmp/interactive-console.log \
  luchs --endpoint=luchs25-interactive --stats --size=800x600 testdata/interactive.html
# Connect one mode at a time, retaining the source between viewers:
capture-viewer-sdl --source-endpoint luchs25-interactive \
  --typing cooperative --affordances required --log-affordances
# Repeat with --typing text and --typing physical.

LUCHS_INPUT_TRACE=1 LUCHS_CONSOLE_LOG=/tmp/page-console.log \
  luchs --endpoint=luchs25-page --stats --size=800x600 testdata/live-acceptance.html
capture-viewer-sdl --source-endpoint luchs25-page \
  --typing cooperative --affordances required --log-affordances
```

Both builds succeeded. The helper build passed SocketRights, snapshot recovery,
URL policy/symlink containment and writer-import tests. `otool -L` confirmed
`@rpath/libjackstay.dylib`; byte comparisons confirmed all three installed files
matched the staged release files. All 19 SDL CTests passed, including typing,
scroll, cursor, presentation and navigation contracts. Those offline tests are
supporting evidence, separate from the live observations below.

| # | Result | Live evidence |
| --- | --- | --- |
| 1 | PASS | Current-main builds, native helper checks, installation of the three matching files, and 19/19 SDL CTests as above. |
| 2 | PASS | In `interactive.html`, the click readout reached `clicks 1`. Cooperative typing plus Cmd+A/C/Right/V produced `CoopCoop`; physical mode produced `PhysicalPhysical`. Text mode inserted `Text` and kept it unchanged during the shortcut sequence, matching its documented suppression of all keys. Screenshots: `cooperative.png`, `text.png`, `text-shortcuts.png`, `physical.png`. |
| 3 | FAIL | Precise scroll moved the long document to y=2089; track clicks moved it to 1549, 1009 and 469. A synthetic two-Line input moved it from 0 to 80. The overlay thumb followed these content changes, but repeated native automated thumb drags left the position unchanged. See the method and failure below. |
| 4 | PASS | Host pointer gestures over the link and input published cursor tags 5 (`pointer`) and 10 (`text`), with 1 (`default`) between them. No page click was needed to establish those hover tags. The SDL log confirms both transitions; screenshots are `hover-link.png` and `hover-text.png`. The screenshots' automation cursor overlay does not independently establish the system cursor shape. |
| 5 | PASS | Clicking the page's title button changed the visible native window title and window-domain snapshot to `Title changed by the page` (`title-retina.png`). |
| 6 | PASS | Toolbar was visible. Typing the second fixture's file URL loaded `Second affordance page`; Back restored the changed title and prior scroll position; Forward returned to the second page; Reload logged another navigation completion. Typing `file:///etc/hosts` left that page visible and logged rejection (`navigation-rejection.png`). |
| 7 | PASS | Resizing from an 800x600 logical viewport to 500x500 changed two card columns to one. Clicking the resized input and typing produced `AfterResize` at the correct field, also recorded in the console (`resize-input.png`). |
| 8 | PASS | The Retina viewer received 1600x1200 pixels for an 800x600 logical viewport, and 1000x1000 after the 500x500 resize. Desktop inspection showed sharp text and borders at those matching drawable sizes (`title-retina.png`, `resize-input.png`). |
| 9 | PASS | While minimized, an independent media observer saw sequence 514 unchanged for 50 seconds while the page's timer advanced. Restoring the viewer resumed publication. The observer requested neither input nor affordances, so it sent no competing presentation hint. |
| 10 | PASS | Each ten-second static run published exactly one frame and skipped every subsequent completed snapshot: 27 skips at 800x600, 28 at 1600x1200. This covers the one-second idle threshold and reports actual `--stats` skips. |
| 11 | PASS | Static and animated stats at both pixel sizes are recorded in the table below. All four runs exited zero. |

### Input, scrolling and navigation excerpts

The native helper trace records cooperative Command-key delivery, including
`code=8` (`c`) and `code=9` (`v`) with `flags=1048576`. The fixture screenshots
show the resulting duplicate strings. Text mode's unchanged `Text` after the
same shortcuts is expected behavior, not an editing failure: that mode sends
committed text and suppresses physical keys, including Command and arrows.

Precise scrolling used desktop automation's synthetic scroll over the actual
SDL window. For the Line case, a standalone C probe connected to the same live
source over bootstrap v2 with required SourceText input, sent Motion at (100,100)
and a `FT_INPUT_SCROLL_LINE` event with y=2, waited for executed completions, and
closed input cleanly. The unchanged SDL viewer was connected with `--observe`
and required affordances during this probe. This verifies Line execution and
its resulting SDL content/overlay state; it does not claim a live non-precise
Cocoa device event through SDL's wheel adapter. That translator has separate
passing CTest coverage; physical device evidence belongs to #17.

```text
# Native precise scroll through SDL:
input scroll dx=-0.0 dy=2640.0 fixed=-2640.0 native=0.0,-2640.0 precise=true
console.log scroll x=0 y=2089
# SDL after the scroll, then three track clicks:
affordances domain=scroll withdrawn=0 x=0/783 y=2089/2689 capabilities=3
affordances domain=scroll withdrawn=0 x=0/783 y=1549/2689 capabilities=3
affordances domain=scroll withdrawn=0 x=0/783 y=1009/2689 capabilities=3
affordances domain=scroll withdrawn=0 x=0/783 y=469/2689 capabilities=3
# Standalone synthetic Line probe, with SDL observing:
geometry=320x180 revision=6
kind=3 scroll_unit=0 y=100.0 sequence=1 outcome=0
kind=5 scroll_unit=2 y=2.0 sequence=2 outcome=0
input_cleanup=completed
# Native helper and SDL:
input scroll dx=0.0 dy=80.0 fixed=-80.0 native=0.0,-80.0 precise=true
console.log scroll x=0 y=80
affordances domain=scroll withdrawn=0 x=0/303 y=80/2957 capabilities=3
# Hover, title, resize and URL refusal:
affordances domain=cursor withdrawn=0 cursor=5
affordances domain=cursor withdrawn=0 cursor=1
affordances domain=cursor withdrawn=0 cursor=10
affordances domain=window withdrawn=0 ready=1 title=Title changed by the page
source frame=1600x1200
console.log viewport 800x600 columns=2
console.log viewport 500x500 columns=1
console.log input AfterResize
source frame=1000x1000
rejected navigation.load: file:///etc/hosts
```

The failed native thumb gestures started inside the visible vertical thumb at
screenshot (1590,940), ending at (1590,250), and at (1590,930), ending at
(1590,700). The 800x600 logical page occupied screenshot x=0..1599,
y=120..1319; at y=1549 the overlay thumb occupied approximately y=811..1079.
Both gestures left y=1549 unchanged. Further attempts at y=469 and at the top
of a 500x500 viewport also produced no changed scroll snapshot. Track clicks
remained effective. All viewer sources were unchanged, and no input rejection
or helper error appeared. Jackstay #87 retains these reproduction steps and
requests a physical mouse drag to distinguish native automation delivery from
a viewer defect. This verification does not assign a root cause or fix it.

### Minimized publication and page clock

A standalone C observer attached to `luchs25-page` using bootstrap v2, no input,
no affordances, and a one-frame holding reservation. It sampled the latest
frame descriptor every 20 ms, released every acquired lease immediately, and
logged sequence and dimensions once per second. Repeated acquisitions of a
retained frame are not counted as new publication.

After minimizing through the native window button, the test inspected Finder
rather than the viewer. Reading the viewer's accessibility state immediately
after minimizing reactivates it in this automation environment and invalidates
the hidden interval; the reported interval avoids that side effect.

```text
# Independent media observer during the uninterrupted minimized interval:
elapsed=104.023 sequence=514 observed_changes=353 pixels=1000x1000
elapsed=154.008 sequence=514 observed_changes=353 pixels=1000x1000
# Helper console during that interval; its timer continued at the background rate:
501.774 console.log timer 49
549.773 console.log timer 73
# After restoring the viewer:
elapsed=171.073 sequence=590 observed_changes=426 pixels=1000x1000
```

The timer runs without publishing its changed pixels while hidden. On final
SIGTERM the source stopped, reaped its helper and exited successfully. The
observation viewer reported `affordances_cleanup=completed`. The long interactive
run's aggregate stats were `snapshots=4981 published=1076 skipped=3905
copies_per_frame=1 mean_snapshot_ms=4.097 mean_publish_ms=1.862`; these combine
navigation, editing, resizing and visibility changes and are not a benchmark.

### Static and animated measurements

Release Luchs and the optimized Swift helper ran at `--fps=30`. Each static
run used `testdata/static.html`, ran for ten seconds from process launch, then
received SIGTERM. Each animated run used `testdata/animated.html` and stopped
at 90 changed publications. Runs were sequential, without a viewer connected
to the measured endpoint. The separate live viewer above establishes Retina
scale-hint delivery; the 1600x1200 CLI runs here reproduce that pixel count
without a host-dependent initial resize.

```sh
luchs --stats --fps=30 --size=800x600 testdata/static.html
luchs --stats --fps=30 --size=1600x1200 testdata/static.html
# Send SIGTERM to each static process after ten seconds.
luchs --stats --fps=30 --frames=90 --size=800x600 testdata/animated.html
luchs --stats --fps=30 --frames=90 --size=1600x1200 testdata/animated.html
```

| Workload | Pixels | Snapshots | Published | Skipped | Copies | Mean snapshot ms | Mean publish ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Static, ten seconds | 800x600 | 28 | 1 | 27 | 1 | 3.279 | 12.064 |
| Static, ten seconds | 1600x1200 | 29 | 1 | 28 | 1 | 5.221 | 6.108 |
| Animated, 90 publications | 800x600 | 90 | 90 | 0 | 1 | 1.264 | 0.952 |
| Animated, 90 publications | 1600x1200 | 90 | 90 | 0 | 1 | 1.444 | 2.953 |

These rows sit alongside the earlier dated tables without replacing them.
The static fixture differs from the earlier one-heading scratch page; its
single cold publication cannot establish steady-state publish throughput.
The animated fixture uses the earlier heading-changing rAF workload, now
committed for reproduction, with a once-per-second console counter. Startup,
background page timers and desktop load are included in this acceptance run;
these timings are not a controlled speedup comparison. `copies_per_frame=1`
remains the implementation's copy-count invariant, not a hardware counter.
