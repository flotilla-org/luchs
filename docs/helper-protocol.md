# Helper protocol

Rust creates one `AF_UNIX` `SOCK_STREAM` socketpair per renderer and passes the
child endpoint in `LUCHS_HELPER_FD`. Only that endpoint survives exec; the helper
sets close-on-exec again before starting WebKit. Stdin and stdout are `/dev/null`
and carry no protocol bytes. Stderr carries diagnostics. The socket can carry
future `SCM_RIGHTS` arena grants (#13); this slice passes no descriptors.

One helper process owns each engine and uses `takeSnapshot`, without Screen
Recording permission. Direct writing into delegated Jackstay arena slots is
follow-on work in #13, depending on Jackstay #75.

## Envelopes and limits

All lengths use unsigned 32-bit little-endian integers. JSON uses UTF-8 and
contains no required trailing newline. A length excludes its own four bytes.
Readers must accept split reads and multiple records in one read. Clean EOF
occurs only between records; a partial prefix or payload is a protocol failure.
A peer reset at a record boundary is treated as EOF, including on Linux when a
helper exits with an unread command. Complete final frames are drained and the
helper's exit status is still checked.

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
with an error for malformed command records and terminates on clean socket EOF.

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
that thread. A serial background socket writer keeps records from interleaving
and keeps blocking writes off WebKit's main thread. The command reader admits
one command at a time and waits for its acknowledgement to drain; a capture's
bitmap stays borrowed until its write completes. This bounds queued work.
While navigation is loading, capture acks immediately without a snapshot or
report, so it cannot hold ping, reload or presentation behind page loading.
Navigation completion reports activity and wakes capture. The existing initial
navigation-failure handlers terminate with a diagnostic; subsequent failures
remain logged. A backing-scale or navigation change during a snapshot discards
that transient result and wakes a retry.
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
| `capture` | Complete a visible snapshot and any changed-frame write before ack; hidden requests take no snapshot |
| `presentation` | Apply `visible` (boolean) and `scale` (positive finite number), validating the resulting pixel size before ack |
| Any other type | Apply no effect; ack `unsupported` |

For a local page, reload reads the file and calls `loadHTMLString` with the page
URL as base URL. A read failure gets `failed`. Remote pages use
`reloadFromOrigin`. An executed reload confirms submission to WebKit, not
navigation completion or delivery of a replacement frame; later navigation
errors go to the console log. Native input and producer-state publication belong to later slices. Old SDL command names currently receive `unsupported`; future input
handlers must deliver native events, with no JavaScript event-synthesis path.

## Rust dispatch and timeouts

One reader thread demultiplexes all socket records. It keeps at most two queued
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

The timeout starts when the command is admitted and includes writing the socket.
Writes use a nonblocking socket; a timeout or write failure terminates and reaps
the helper because a partial command cannot be retried on the same stream. Socket
readiness uses `poll`; frame reception retains the write error even if socket
EOF races shutdown.
`Helper::command` maps send failure, timeout, and helper disconnection to
`CommandOutcome::Uncertain`. A reader accepts an ack only before its command's
deadline; even a caller that waits later cannot turn a late ack into `Executed`.
An ack queued before the deadline retains its outcome.

Unmatched acks, including late acks and duplicate acks, are discarded. They
cannot satisfy a different waiter or revise an outcome already returned. This
keeps retired-ID bookkeeping bounded; helpers still owe exactly one ack for
each command. `Helper::ignored_acks()` counts these discarded replies, and the
CLI logs a nonzero count at shutdown. `execution_outcome()` maps `executed` to Jackstay `Executed`,
`unsupported` to `Unsupported`, `failed` to `Rejected`, and timeout/disconnection
to `Uncertain`. The CLI uses a one-second reload deadline. Unsupported reload
is fatal because it means the helper cannot implement watch. Failed or uncertain
reloads log a diagnostic when the failure outcome changes and retain the last successfully handled modification
time, retrying on the next 250 ms poll even if the file has not changed again.
Malformed output and command-write failures remain fatal. `Uncertain` does not
prove that no effect occurred: watch can apply the same reload request more than
once. Future non-idempotent input or affordance commands must not use this retry
policy.

## Frames

The JSON header uses device-pixel dimensions and premultiplied BGRA bytes:

```json
{"format":"bgra8","width":800,"height":600,"stride":3200,"len":1920000}
```

Width and height must be positive unsigned 32-bit integers. Stride must be at
least `width * 4`, and `len` must equal `stride * height`, fit the 64 MiB cap,
and match the bytes remaining in the record. The format must be `bgra8`.
No record can contain trailing bytes after its declared pixels.

The Swift helper emits little-endian premultiplied BGRA (`Bgra8Unorm`). It sets
`snapshotWidth` to `logical_width * scale / window_backing_scale`, then draws
WebKit's native image representation 1:1 into a reused bitmap/context. Integer
pixel dimensions round each logical dimension times scale to the nearest pixel.
The original representation preserves odd sizes which screen-scaled `NSImage`
CGImage extraction would round away. There is no channel swizzle, intermediate
pixel `Data`, or per-frame page mutation. The CGContext uses the snapshot's RGB
colour space, determined when allocating a size.

Bitmap storage, its graphics contexts and frame envelope are reused until the
pixel size changes. Rust decodes headers with fixed scratch storage and recycles
pixel vectors from mailbox drops, replaced latest frames and the toolkit's
`recycle()` callback. Frame-sized allocations occur during warm-up or size growth;
small command/ack JSON and WebKit's snapshot objects still use framework storage.
The toolkit's `input_size()` callback keeps input geometry in the logical viewport
when scale changes the pixel allocation. The Swift logical viewport stays fixed;
preferred size and focus hints remain #7's work.

## Capture policy and reports

Rust requests one capture at a time, using the ordinary command IDs and acks.
A successful capture ack adds an optional report without changing its outcome:

```json
{"id":4,"outcome":"executed","capture":{"published":false,"snapshot_ns":2100000,"publish_ns":0}}
```

`published` means the helper sent a changed frame. Snapshot time measures the
WebKit API latency; publish time includes drawing, hashing and draining a changed
frame to the socket. Skipped frames have zero publish time. A hidden or loading capture ack
has no report because no snapshot was taken. `--stats` reports completed snapshots,
Jackstay publications, unchanged skips and the mean times at exit. An in-flight
capture interrupted by shutdown is not counted as completed.

The helper compares an FNV-1a fingerprint of native pixels with its last sent
frame. A match emits no frame record. Rust also fingerprints dimensions, stride
and pixels before Jackstay publication, guarding against renderer duplicates.
After one second without a changed frame or wake, captures back off from `--fps`
(default 30) to 2 fps. Deadlines run from snapshot completion, so the effective
frame rate is lower than `--fps` by snapshot and publish latency. The Rust loop
sleeps until the next capture or 250 ms watch/signal deadline; acks, page activity
and presentation callbacks interrupt its condition-variable wait immediately.
A changed frame restores full rate. Reload and presentation
commands wake capture; every non-capture helper command also reports a wake.
Page mutations, editing, selection, focus, scroll and resize report
`{"capture_changed":true}` on the existing state tag. Active CSS animations report
activity from a page rAF probe started by animation/transition events or DOM
mutations. The probe stops when no animation is running, so static pages have
no continuous animation-probe work. The
caret updates from page events, with a deferred rAF after native default actions.
Canvas/video changes without a DOM or animation signal are discovered by the idle
probe, within its 500 ms interval, then restore full rate.

The toolkit's `affordance()` callback forwards host presentation hints. Last
received hints win; withdrawal or channel closure restores defaults. An oversized
scale hint is logged and retains the previous scale; its visibility still applies. `visible=false` stops capture requests and
suppresses queued publications while leaving the WebKit window ordered in, so
its page clock keeps running. `visible=true` requests an immediate capture and
invalidates the previous fingerprint so an unchanged image can be presented
again. `--frames=N` counts changed helper frames; a static page with N greater
than one can therefore remain running until content or presentation changes.

## Verification

`cargo test --locked --test helper_protocol --test cli` exercises fake-helper
frame passthrough, split records, out-of-order acks under frame backpressure,
unknown commands, failed commands, timeout and late acks, state callbacks,
malformed records, size bounds, callback panic, process reaping, and CLI watch
reload recovery after failed and uncertain acks (including unsupported failure).
`capture_policy` exercises the real CLI with a fake socketpair renderer: duplicate
suppression, idle threshold, reload and page-signal wake, scale through the actual
presentation callback, hidden capture suspension and immediate re-show.
Fake command helpers require `python3`, available on the CI runners.

On a logged-in macOS desktop, build the Swift helper and run:

```sh
scripts/build-helper.sh
cargo test --locked --test live_macos -- --ignored --nocapture
```

The tests check slow-loading capture/ping/presentation responsiveness and initial navigation failure, native ping/reload acks, a full-range u64 ID, zero-status exit on
socket EOF, and verify that `--watch` publishes
changed page pixels through the real CLI, Swift helper, and Jackstay producer.
See [live macOS evidence](live-macos.md) for the recorded run.
