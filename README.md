# Luchs

Luchs is a page viewer that publishes frames as a Jackstay source. Its Rust core
owns the CLI, renderer process and CPU producer; the first renderer is the
macOS `WKWebView` helper extracted from Katzensteg. Any Jackstay consumer can
present the page. SDL is a reference consumer, not Luchs's primary presenter.

This repository implements the frame-producing slice of
[Jackstay's split](https://github.com/flotilla-org/jackstay/issues/32).
Input admission and affordances belong to later slices. Every connection is
observation-only, including one that requests optional input. Required input
fails admission. The helper retains its existing stdin protocol, but this core
only sends reload commands.

## Build and install

Use Rust/Cargo and, on macOS, Xcode Command Line Tools with `swiftc`:

```sh
cargo build --release --locked
scripts/build-helper.sh target/release
install -d "$HOME/.local/bin"
install target/release/luchs target/release/luchs-webview-capture "$HOME/.local/bin/"
```

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

Luchs prints its source socket path on stdout and diagnostics on stderr. Pass
that path to a consumer built with Jackstay source bootstrap support:

```sh
katzensteg jackstay-source /path/printed/by/luchs.sock --observe
capture-viewer-sdl --source-socket /path/printed/by/luchs.sock --observe
```

The endpoint lives in Jackstay's private per-user runtime directory and the
socket mode is `0600`. Its default name is `luchs-<pid>`; `--endpoint` chooses a
name, not an arbitrary socket path. Jackstay checks peer ownership and Luchs
runs `jackstay::bootstrap::accept` without an input target before CPU setup.
Consumers may join, leave, or die while the renderer keeps running. The producer
has eight resources, one retained frame, one producer reserve and at most three
consumer incarnations; admission also depends on each consumer's holding credit.

`--size=WxH` defaults to 800x600. `--watch` polls a local file's modification time
every 250 ms and asks WebKit to reload without its cache; URLs load once.
`LUCHS_CONSOLE_LOG` names the helper's console/error/navigation log, otherwise
the helper uses `/tmp/luchs-console-<helper-pid>.log`. Its stderr passes through
to Luchs's stderr. `--renderer=native-webview` remains accepted for existing
hosts, and a bare `--` separates options from the page. `--fps` defaults to 30;
`--frames=N` bounds a smoke run, with zero meaning unbounded.

The imported helper is unchanged. It uses the persistent website data store,
a transparent on-screen window so WebKit keeps its page clock running, Safari's
user agent, popup views with their opener, and the existing caret script.
Frames come from `takeSnapshot`, not screen capture. The helper emits
premultiplied RGBA; consumers that assume straight alpha can darken translucent
content, an inherited limitation. A live macOS desktop session is required.

The Swift viewport stays fixed for a run. The Rust protocol accepts changes in
frame dimensions or stride and reconfigures the Jackstay CPU allocation before
publishing the replacement. A capacity-paused replacement waits for retirement;
it never overwrites leased pixels. Frames are limited to 64 MiB and the producer's
allocation budget is 1 GiB. SIGINT, SIGTERM, EOF and errors all close the endpoint
and stop/reap the helper. Consumer shutdown does not terminate Luchs.

## Verification

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo +1.98.0 fmt --all --check
```

CI runs these checks on macOS and Linux, plus helper compilation on macOS.
Tests spawn fake helpers without WebKit and exercise the frame stream, invalid
headers, truncation, reload, console environment, termination, source bootstrap,
replacement and consumer process death. One ignored test is a child-process
fixture invoked by the death test, not a skipped acceptance check.

See [source history](docs/source-history.md) for the filtered import and commit
map, and [live macOS evidence](docs/live-macos.md) for presenter validation.
