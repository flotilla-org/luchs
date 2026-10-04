# Helper protocol

Luchs owns a renderer subprocess with piped stdin and stdout. This contract
applies to the macOS WKWebView helper and future WebKitGTK and WebView2 helpers.
Stderr carries diagnostics; stdout carries only protocol records. There is no
JSON-line input, `LUCHS_RAW_FRAME` prefix, or JavaScript input bridge.

## Envelopes and limits

All lengths use unsigned 32-bit little-endian integers. JSON uses UTF-8 and
contains no required trailing newline. A length excludes its own four bytes.
Readers must accept split reads and multiple records in one read. Clean EOF
occurs only between records; a partial prefix or payload is a protocol failure.

| Direction | Envelope | Limit |
| --- | --- | --- |
| Rust to helper | `u32 json_length`, then JSON bytes | 1 to 131,072 JSON bytes |
| Helper to Rust | `u32 record_length`, `u8 tag`, then payload | Limits below include the tag |
| Frame (tag 1) | `u32 header_length`, JSON header, raw pixels | At most 4,096 header bytes and 67,108,864 pixel bytes; outer maximum 67,112,965 bytes |
| Ack (tag 2) | JSON object | At most 131,072 record bytes |
| State (tag 3) | JSON object | At most 131,072 record bytes |

There is no padding between records. Zero lengths, unknown output tags, invalid
JSON, invalid fields, oversized records, and truncation are fatal. Reject a
length before allocating its payload. Frame readers validate the header before
allocating pixels. On failure Rust terminates and reaps the helper, disconnects
ack waiters, and reports the error through frame reception. The helper exits
with an error for malformed stdin and terminates on clean stdin EOF.

## Commands and acknowledgements

A command is an object with an unsigned 64-bit `id` and a nonempty string `type`:

```json
{"id":1,"type":"reload"}
```

IDs belong to one helper lifetime. Rust starts at 1, increments for every sent
command, and never reuses an ID. Implementations must preserve all 64 bits;
converting through a floating-point number loses IDs above 2^53. Unknown object
fields are reserved for extensions.

The helper emits exactly one ack per valid command, including unknown command
types. It applies the command on its UI main thread and then emits the ack on
that thread. Serial stdout ownership prevents frames and acks from interleaving.
An `executed` ack confirms actual application of the command; enqueueing work
for another thread is not execution.

```json
{"id":1,"outcome":"executed"}
{"id":2,"outcome":"unsupported"}
{"id":3,"outcome":"failed","detail":"could not read local page"}
```

`outcome` is exactly `executed`, `unsupported`, or `failed`. `detail` is an
optional string (or null); it explains a failure without replacing the outcome.
An invalid ack schema is a protocol failure.

| Command | Effect before ack |
| --- | --- |
| `ping` | The main thread has handled the command; ack `executed` |
| `reload` | Apply a cache-bypassing reload request to the main page view; ack `executed` after the WebKit load/reload call returns |
| Any other type | Apply no effect; ack `unsupported` |

For a local page, reload reads the file and calls `loadHTMLString` with the page
URL as base URL. A read failure gets `failed`. Remote pages use
`reloadFromOrigin`. An executed reload confirms submission to WebKit, not
navigation completion or delivery of a replacement frame; later navigation
errors go to the console log. Input and affordance commands belong to later
slices. Old SDL command names currently receive `unsupported`; future input
handlers must deliver native events, with no JavaScript event-synthesis path.

## Rust dispatch and timeouts

One reader thread demultiplexes all stdout records. It keeps at most two queued
frames, dropping the oldest when full so frame reception cannot block ack
routing. `Helper::dropped_frames()` counts mailbox drops; the CLI logs the
count at shutdown when nonzero. `Helper::spawn_with_state` delivers state objects
to a callback on that reader thread; the callback must return promptly. A
callback panic terminates and reaps the helper and reports a stream error. This slice
reserves the tag and accepts opaque objects. Issue #3 will define their fields.

`Helper::send_command` registers a per-ID waiter before writing any bytes and
returns a `PendingCommand`. Multiple commands can be outstanding and acks can
arrive in any order. At most 64 commands can await acknowledgement; further
admission returns `WouldBlock`. Dropping a waiter frees its slot.

The timeout starts when the command is admitted and includes writing stdin.
Writes use a nonblocking pipe; a timeout or write failure terminates and reaps
the helper because a partial command cannot be retried on the same stream.
`Helper::command` maps send failure, timeout, and helper disconnection to
`CommandOutcome::Uncertain`. A reader accepts an ack only before its command's
deadline; even a caller that waits later cannot turn a late ack into `Executed`.
An ack queued before the deadline retains its outcome.

Unmatched acks, including late acks and duplicate acks, are discarded. They
cannot satisfy a different waiter or revise an outcome already returned. This
keeps retired-ID bookkeeping bounded; helpers still owe exactly one ack for
each command. `execution_outcome()` maps `executed` to Jackstay `Executed`,
`unsupported` to `Unsupported`, `failed` to `Rejected`, and timeout/disconnection
to `Uncertain`. The CLI uses a one-second reload deadline. Unsupported reload
is fatal because it means the helper cannot implement watch. Failed or uncertain
reloads log a diagnostic and retain the last successfully handled modification
time, retrying on the next 250 ms poll even if the file has not changed again.
Malformed output and command-write failures remain fatal.

## Frames

The JSON header retains the existing RGBA fields:

```json
{"format":"rgba8","width":800,"height":600,"stride":3200,"len":1920000}
```

Width and height must be positive unsigned 32-bit integers. Stride must be at
least `width * 4`, and `len` must equal `stride * height`, fit the 64 MiB cap,
and match the bytes remaining in the record. The format must be `rgba8`.
No record can contain trailing bytes after its declared pixels.

The Swift helper emits premultiplied RGBA, as before. Frame envelopes contain
raw pixels rather than base64; dimensions and stride may change between frames.
The current Swift viewport stays fixed during a run.

## Verification

`cargo test --locked --test helper_protocol --test cli` exercises fake-helper
frame passthrough, split records, out-of-order acks under frame backpressure,
unknown commands, failed commands, timeout and late acks, state callbacks,
malformed records, size bounds, callback panic, process reaping, and CLI watch
reload recovery after failed and uncertain acks (including unsupported failure).
Fake command helpers require `python3`, available on the CI runners.

On a logged-in macOS desktop, build the Swift helper and run:

```sh
scripts/build-helper.sh
cargo test --locked --test live_macos -- --ignored --nocapture
```

The tests check native ping/reload acks, a full-range u64 ID, zero-status exit on
stdin EOF, and verify that `--watch` publishes
changed page pixels through the real CLI, Swift helper, and Jackstay producer.
See [live macOS evidence](live-macos.md) for the recorded run.
