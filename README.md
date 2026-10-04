# Luchs

Luchs is a page viewer that publishes frames as a Jackstay source. Its Rust core
owns the CLI, renderer process and CPU producer; the first renderer is the
macOS `WKWebView` helper extracted from Katzensteg. A bootstrap v2 Jackstay
consumer can present the page. SDL is a reference consumer, not Luchs's primary presenter.

This repository implements the frame-producing slice of
[Jackstay's split](https://github.com/flotilla-org/jackstay/issues/32).
Luchs advertises cooperative, text and physical typing, pointer and scroll input
through the producer toolkit's ordered executor. Commands use the
[framed helper protocol](docs/helper-protocol.md). Only an `executed` helper ack
completes input successfully; unsupported mappings are rejected, and failed or
missing acks are uncertain. One controller owns the target. Focus loss,
disconnect and shutdown wait for native releases before admitting a replacement.

Page state publishes window, navigation, cursor and document-scroll affordances.
Navigation and scroll verbs execute in WebKit; host presentation scale and
visibility hints control capture.

## Build and install

Use Rust/Cargo and, on macOS, Xcode Command Line Tools with `swiftc`:

```sh
cargo build --release --locked
scripts/build-helper.sh target/release
install -d "$HOME/.local/bin"
install target/release/luchs target/release/luchs-webview-capture "$HOME/.local/bin/"
```

The helper build also validates its native recovery policy. Set
`LUCHS_SKIP_NATIVE_TESTS=1` when building to skip that extra test compilation;
`scripts/test-helper.sh` runs it separately.

The helper must sit beside the `luchs` executable. `--helper PATH` overrides its
location, including for fake helpers on Linux. The Rust workspace builds and
tests on macOS and Linux without WebKit; only `scripts/build-helper.sh` compiles
the Swift renderer. Linux has no real renderer yet.

Jackstay is pinned to a full git revision in the workspace `Cargo.toml`, with
resolved dependencies committed in `Cargo.lock`.

## Run a page

```sh
luchs --endpoint=my-page --size=800x600 --watch testdata/interactive.html
luchs --endpoint=my-web-page https://example.com
```

Luchs prints its source socket path on stdout and diagnostics on stderr. Connect
the SDL viewer by the configured endpoint name, using the pinned ABI 0.13 build:

```sh
capture-viewer-sdl --source-endpoint my-page --typing cooperative --affordances optional
```

Bootstrap v2 is required. Build the SDL viewer against the exact Jackstay revision
pinned here; use `--source-endpoint` for its v2 path. Add `--observe` for frames
without input, or choose `--typing text` / `--typing physical` to exercise those
modes. V1-only consumers cannot connect.
Rust consumers use `jackstay::bootstrap::connect_v2` with optional or no controls.

The endpoint lives in Jackstay's private per-user runtime directory and the
socket mode is `0600`. Its default name is `luchs-<pid>`; `--endpoint` chooses a
name, not an arbitrary socket path. Jackstay checks peer ownership and Luchs
uses `jackstay_producer::Builder` and bootstrap v2 before CPU setup.
Consumers may join, leave, or die while the renderer keeps running. The producer
has eight resources, one retained frame, one producer reserve and at most three
arena incarnations, with `max_connections=16` bounding simultaneous toolkit
workers (including stalled handshakes and independent controls). Arena admission
also depends on each consumer's holding credit.

`--size=WxH` defaults to 800x600. `--watch` polls a local file's modification time
every 250 ms and asks WebKit to reload without its cache, waiting up to one
second for its execution ack. Failed or uncertain reloads log and retry on the
next poll; an unsupported reload fails the run. URLs load once.
`LUCHS_CONSOLE_LOG` names the helper's console/error/navigation log, otherwise
the helper uses `/tmp/luchs-console-<helper-pid>.log`. Its stderr passes through
to Luchs's stderr. `--renderer=native-webview` remains accepted for existing
hosts, and a bare `--` separates options from the page. `--fps` defaults to 30;
`--frames=N` bounds a smoke run, with zero meaning unbounded.

The helper uses the persistent website data store,
a transparent on-screen window so WebKit keeps its page clock running, Safari's
user agent, popup views with their opener, and the existing caret script.
Frames come from `takeSnapshot`, not screen capture. The helper emits BGRA
with premultiplied alpha (`Bgra8Unorm`); consumers must respect premultiplication
when compositing translucent pixels. It reuses its bitmap and contexts and writes
changed pixels on a background queue through an inherited socketpair. Rust pools
frame storage and returns consumed buffers through the producer toolkit.
A live macOS desktop session is required.

Each snapshot is compared with the previous publication. Unchanged pixels are
skipped, and capture backs off to 2 fps after one second idle. Commands and page
activity restore full rate. `visible=false` pauses snapshots while WebKit's window
stays ordered in; showing it again captures immediately. A `scale` hint requests
rounded `width * scale` by `height * scale` device pixels and draws them 1:1,
including Retina and odd fractional sizes. Input coordinates stay logical.
`--stats` logs completed snapshots, published frames, unchanged skips and mean
snapshot/publish times at shutdown. `--frames` counts changed frames, so a static
page need not reach a limit greater than one.

The renderer architecture stays one engine per helper. ScreenCaptureKit and
capturing the helper window through Porthole are not the frame path; guaranteed
GPU capture would require a different engine. Direct writing into delegated
Jackstay arena slots remains #13, after Jackstay #75. The socketpair is the
future descriptor-transfer channel.

The Swift viewport stays fixed for a run. The Rust protocol accepts changes in
frame dimensions or stride and reconfigures the Jackstay CPU allocation before
publishing the replacement. The toolkit manages capacity-paused replacements,
retrying reconfiguration on later helper frames as old allocations retire;
it never overwrites leased pixels. The callback keeps only the latest helper
frame, so intermediate frames may be skipped. Shutdown allows up to one second
for the final queued frame before starting ordered toolkit teardown. Frames are limited to 64 MiB and the producer's
allocation budget is 1 GiB. SIGINT, SIGTERM, EOF and errors all close the endpoint
and stop/reap the helper. The toolkit drains for up to five seconds; consumers
must retire their mappings and leases when media closes. Drain or input cleanup
failures are reported. Consumer shutdown does not terminate Luchs.

## Page affordances

Use `--affordances required --log-affordances` in the SDL viewer to inspect page
state and enable its navigation toolbar and document scrollbar overlays.
`testdata/affordances.html` has links, a text field, a title-change button, and
both scroll axes. Window readiness latches after the first completed navigation
and published frame for a helper lifetime. Later loads update navigation loading;
helper replacement resets readiness. Titles, history and loading follow the active WebKit view,
including popups. Cursor changes follow the last pointer position in the page;
host pointer motion reaches WebKit through the native input executor.

Only `document.scrollingElement` is represented. Scroll positions and dimensions
use CSS pixels; nested scrollers are excluded. Small steps are 40 pixels and
large steps are 90% of that axis's viewport. Positions are clamped before
publication and execution, including non-scrollable axes, which publish zero.
Restored history pages republish state on `pageshow`.

Host `navigation.load` URLs are untrusted. HTTP and HTTPS are allowed. File URLs
must resolve to an existing path inside the original local page's canonical
directory; symlinks outside it are rejected. A remote startup page grants no
file access. Rejections have no host reply and are recorded in `LUCHS_CONSOLE_LOG`.
Unknown verbs are ignored. Complete domain snapshots replace earlier state;
helper replacement withdraws all four domains and clears pending commands.
The CLI still exits on helper EOF rather than automatically restarting it.

## Verification

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo +1.98.0 fmt --all --check
```

CI runs these checks on macOS and Linux, plus helper compilation on macOS.
Tests spawn fake helpers without WebKit and exercise framed records, ack
ordering and timeouts, state callbacks, bounds, reload, console environment,
termination, source bootstrap, replacement, pooled storage, idle policy,
presentation hints, page-state propagation, helper replacement, URL containment,
navigation/scroll command forwarding and consumer process death. Command
fixtures require `python3`. The ignored `live_macos` test requires a built Swift
helper and a logged-in desktop; a separate ignored child-process fixture runs
inside the consumer-death test.

See [source history](docs/source-history.md) for the filtered import and commit
map, and [live macOS evidence](docs/live-macos.md) for presenter validation.
