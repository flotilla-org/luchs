# Helper protocol

Rust creates one `AF_UNIX` `SOCK_STREAM` socketpair per renderer and passes the
child endpoint in `LUCHS_HELPER_FD`. Only that endpoint survives exec; the helper
sets close-on-exec again before starting WebKit. Stdin and stdout are `/dev/null`
and carry no protocol bytes. Stderr carries diagnostics. The socket carries one `SCM_RIGHTS` payload descriptor with each `arena` command.
No pixel bytes travel over the socket.

One helper process owns each engine and uses `takeSnapshot`, without Screen
Recording permission. Rust reserves and commits Jackstay CPU slots; the helper maps only their payload
object and draws snapshots directly into the reserved slot. It does not link Jackstay.

## Envelopes and limits

All lengths use unsigned 32-bit little-endian integers. JSON uses UTF-8 and
contains no required trailing newline. A length excludes its own four bytes.
Readers must accept split reads and multiple records in one read. Clean EOF
occurs only between records; a partial prefix or payload is a protocol failure.
A peer reset at a record boundary is treated as EOF, including on Linux when a
helper exits with an unread command. Outstanding draws are abandoned on EOF and the helper's exit status is still checked.

| Direction | Envelope | Limit |
| --- | --- | --- |
| Rust to helper | `u32 json_length`, then JSON bytes | 1 to 131,072 JSON bytes |
| Helper to Rust | `u32 record_length`, `u8 tag`, then payload | Limits below include the tag |
| Retired frame tag 1 | Rejected | No socket pixel fallback |
| Ack (tag 2) | JSON object | At most 131,072 record bytes |
| State (tag 3) | JSON object | At most 131,072 record bytes |

There is no padding between records. Zero lengths, unknown output tags, invalid
JSON, invalid fields, oversized records, and truncation are fatal. Reject a
length before allocating its payload. Draw acknowledgements carry a frame header, validated against the reserved
layout before commit. On failure Rust terminates and reaps the helper, disconnects
ack waiters, and reports the error through the event wait. The helper exits
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
one command at a time and waits for its acknowledgement to drain; a draw owns its reserved slot until its acknowledgement drains. This bounds queued work.
While navigation is loading, capture acks immediately without a snapshot or
report, so it cannot hold ping, reload or presentation behind page loading.
Navigation completion reports activity and wakes capture. The existing initial
navigation-failure handlers terminate with a diagnostic; subsequent failures
remain logged. A backing-scale or navigation change during a snapshot discards
that transient result and wakes a retry. An unexpected representation size likewise
discards the result, invalidates the configuration and requests another capture.
WebKit errors, missing images and invalid snapshot dimensions share a budget of
two retries. A third consecutive failure emits a failed ack with a diagnostic
that the CLI displays. A successful draw, including an unchanged image, resets
the budget. Expected backing-scale and navigation transitions do not consume
that budget; captures remain suspended while loading. Discarded attempts acknowledge without a snapshot report; the ack
reschedules the normal interval and activity wakes it immediately.
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
| `arena` | Receive exactly one payload fd, map `layout.map_len` bytes read/write, unmap the previous generation, then ack with `generation` |
| `arena_release` | Unmap the named generation and destroy its contexts, then ack with that generation |
| `draw` | Validate `slot`, `generation`, `width`, `height`, `stride`; snapshot and draw into that reserved slot; ack with matching generation/slot and a changed frame header, an unchanged report, or no report for a discarded/hidden/loading attempt |
| `presentation` | Apply `visible` (boolean) and `scale` (positive finite number), validating the resulting pixel size before ack |
| `mouse_move`, `mouse_down`, `mouse_up` | Deliver a native pointer event with f64 `x`/`y`; down/up require canonical `button` 1 through 5 |
| `scroll` | Deliver a continuous pixel CGEvent with `dx`/`dy` and accumulated integer `point_dx`/`point_dy` |
| `key_down`, `key_up` | Apply a key operation with `press`, `key_code`, `modifiers`, `repeat`, `cooperative`, and optional `logical` characters |
| `text` | Apply UTF-8 `text`; `cooperative=true` pairs a deferred printable press, otherwise uses the web view's `NSTextInputClient.insertText` |
| `cleanup` | `scope=pointer` releases held buttons; `scope=all` also releases held keys and discards undelivered printable presses, before ack |
| `navigation.back`, `navigation.forward`, `navigation.reload`, `navigation.stop` | Apply the matching method to the active WKWebView |
| `navigation.load` | Validate `url` and submit the load to the active view; rejected loads log and ack internally without replying to the host |
| `navigation.rejected` | Log a load rejected by Rust's URL policy; no navigation |
| `scroll.set_position` | Clamp `position` on `axis` (`x` or `y`) to the current document range, then `scrollTo` |
| `scroll.scroll_by_step` | Move `axis` by 40 CSS px (`step: small`) or 90% of its viewport (`large`), with `direction: increment/decrement` |
| Any other type | Apply no effect; ack `unsupported` |

For a local page, reload reads the file and calls `loadHTMLString` with the page
URL as base URL. A read failure gets `failed`. Remote pages use
`reloadFromOrigin`. An executed reload confirms submission to WebKit, not
navigation completion or delivery of a replacement frame; later navigation
errors go to the console log. Navigation verbs use the active view;
`navigation.reload` calls `reloadFromOrigin`, while the file-watch `reload`
command retains its original main-page behavior. Native input uses no
JavaScript event-synthesis path or SDL keycode vocabulary.

## Native input

Rust's `keymap.rs` maps DOM physical codes to Carbon virtual key codes. Unknown
codes are unsupported without a guess. Logical keys carry literal characters or
AppKit's named-key characters. For a printable logical character without a
known physical position, `key_code=65535` (`u16::MAX`) explicitly means no
physical binding. It is used only with literal `logical` characters, retained
for release, and never sent to `UCKeyTranslate` or guessed as another key. The
helper passes this sentinel to `NSEvent.keyEvent` so native text delivery cannot
accidentally select a physical-key shortcut. The helper derives physical `characters` and
`charactersIgnoringModifiers` from the current layout with
[UCKeyTranslate](https://developer.apple.com/documentation/coreservices/1390584-uckeytranslate).
It retains code, characters, original flags and destination view per press.
Repeats and releases resolve that binding rather than translating a new position.
Modifier metadata uses Jackstay's shift/control/alt/super/caps-lock bits; physical
modifier presses produce `flagsChanged`. Standard Edit menu key equivalents
route native Cmd+A/C/V/X/Z to the bound web view.

Cooperative printable down commands reserve a native binding and await the
following text commit. Text dispatches one native keyDown carrying the commit.
An unrelated key down flushes the pending key; its own up flushes it if no text
arrived. Cleanup drops a pending press that was never delivered. Text mode calls
`insertText` directly and produces no synthetic key events. Native responder
calls return before `executed` is sent; page processing in WebKit's separate
process can finish afterward.

Pointer coordinates stay in logical viewport units regardless of capture scale;
the helper converts to window points without integer rounding. Buttons are
primary 1, secondary 2, auxiliary 3, back 4 and forward 5; native button numbers
are 0 through 4. Double-click count uses the native interval, button and distance.
Motion coalescing belongs to Jackstay. Holds retain their destination view, so
cleanup still releases a press if the active popup changes.

Scroll follows [Jackstay #64](https://github.com/flotilla-org/jackstay/issues/64):
Pixel uses logical points, Line multiplies by 40, and Page by logical viewport
height. Positive y moves content toward its end and positive x toward the right;
the helper negates both for Quartz. Rust retains subpixel remainders per axis for
the integer point fields. Every event also carries its own fractional displacement
in the signed 16.16
[fixed-point fields](https://developer.apple.com/documentation/coregraphics/cgeventfield).
Set line fields first, then fixed-point fields, then point fields: Quartz's line
setter otherwise overwrites point values. `NSEvent` exposes integer point deltas
from this CGEvent path, so four 0.25-point events deliver one point without loss.
Unsupported values outside the signed fixed-point range are rejected. Cleanup
resets remainders after its executed ack. Scroll carry advances only after an
executed ack; failed sends, rejection and uncertainty leave the prior carry
unchanged. Phases and momentum metadata are deferred by the v1 contract.

The toolkit owns exclusive controller admission and cleanup barriers. Rust sends
one scoped cleanup command and waits for the helper to release every hold before
acknowledging it. Failed or timed-out cleanup quarantines replacement admission.
Geometry changes trigger pointer cleanup through the toolkit. A capture-scale
change retains logical geometry and therefore does not cancel holds.

## Page state records

Each page event publishes one complete domain body, never a delta:

```json
{"domain":"window","body":{"title":"Page title","requested_size":null,"ready":true}}
{"domain":"navigation","body":{"url":"https://example.com/","title":"Page title","can_go_back":false,"can_go_forward":false,"loading":false,"capabilities":{}}}
{"domain":"cursor","body":{"shape":"pointer"}}
{"domain":"scroll","body":{"x":{"scrollable":false,"content_length":800,"viewport_length":800,"position":0},"y":{"scrollable":true,"content_length":2200,"viewport_length":600,"position":400},"capabilities":{}}}
```

Rust supplies the navigation and scroll capability flags. The helper observes
WKWebView title, URL, history and loading with KVO. Readiness latches after the
first navigation finishes and a frame reaches the producer toolkit. The Rust
bridge treats helper readiness as advisory and also gates it on frame publication. The active popup owns these
domains until it closes, then the main view republishes its state.

The main-frame script reads only `document.scrollingElement`, in CSS pixels.
Scroll, resize, mutation, resource load and ResizeObserver notifications coalesce
into one animation-frame update. History-cache `pageshow` forces republication.
Overflow hidden/clip axes publish zero and are not scrollable. Content/viewport
lengths remain available. Rust clamps all positions again before publishing.

The cursor script computes CSS under the last page pointer position and posts
only when the resolved shape changes. Auto resolves to pointer on links, text
on editable content or text hit by the pointer, and default elsewhere. Unknown
shapes become default. Host pointer motion reaches the script through native
WebKit input events.

Host navigation/scroll verbs enter a bounded 64-command queue; the CLI dispatches
one verb at a time without waiting on WebKit in the producer callback. Helper
acks are internal and never become host replies. Unsupported verbs are ignored.
When replacing a helper, `Source::helper_stopped` clears page commands and
withdraws all four domains before new state is published. CLI EOF abandons any
outstanding draw, checks the helper exit status, then closes the source.

Rust parses host loads as URLs and canonicalizes file paths under the startup
page directory. The helper independently validates schemes and symlink
containment at the engine boundary. HTTP/HTTPS may target any host, including from a local startup page; remote
startup pages grant no file authority. Rejected loads log to the helper console file.

## Rust dispatch and timeouts

One reader thread demultiplexes acknowledgement and state records.
`Helper::spawn_with_state` delivers state objects to a callback on that reader
thread; the callback must return promptly. A callback panic terminates and reaps
the helper and reports a stream error. State objects use the complete domain
schema above; the page-state bridge ignores unrelated objects.

`Helper::send_command` registers a per-ID waiter before writing any bytes and
returns a `PendingCommand`. Multiple commands can be outstanding and acks can
arrive in any order. At most 64 commands can await acknowledgement; further
admission returns `WouldBlock`. Dropping a waiter frees its slot.

The timeout starts when the command is admitted and includes writing the socket.
Writes use a nonblocking socket; a timeout or write failure terminates and reaps
the helper because a partial command cannot be retried on the same stream. Socket
readiness uses `poll`; the event wait retains the write error even if socket
EOF races shutdown.
`Helper::command` maps send failure, timeout, and helper disconnection to
`CommandOutcome::Uncertain`. A reader accepts an ack only before its command's
deadline; even a caller that waits later cannot turn a late ack into `Executed`.
An ack queued before the deadline retains its outcome.

Unmatched acks, including late acks and duplicate acks, are discarded. They
cannot satisfy a different waiter or revise an outcome already returned. This
keeps retired-ID bookkeeping bounded; helpers still owe exactly one ack for
each command. `Helper::ignored_acks()` counts these discarded replies, and the
CLI logs a nonzero count at shutdown. `receive_event` distinguishes wake, deadline
and clean closure with `HelperEvent`; malformed records return an I/O error. `execution_outcome()` maps `executed` to Jackstay `Executed`,
`unsupported` to `Unsupported`, `failed` to `Uncertain`, and timeout/disconnection
to `Uncertain`. The CLI uses a one-second reload deadline. Unsupported reload
is fatal because it means the helper cannot implement watch. Failed or uncertain
reloads log a diagnostic when the failure outcome changes and retain the last successfully handled modification
time, retrying on the next 250 ms poll even if the file has not changed again.
Malformed output and command-write failures remain fatal. `Uncertain` does not
prove that no effect occurred: watch can apply the same reload request more than
once. Future non-idempotent input or affordance commands must not use this retry
policy.

## Arena grants and draws

At setup Rust exports Jackstay's writable payload object. An `arena` command
carries its descriptor and exactly one fd in `SCM_RIGHTS`, attached to the first
command byte. Ancillary receipt uses `recvmsg` even across split reads. The helper
validates the object length and layout, maps it shared read/write, closes the fd,
and acknowledges its allocation generation. No Jackstay bookkeeping is exported.
`arena_scope` is carried in the layout; the helper uses the generation and slot
bounds within this one process lifetime.

```json
{"id":1,"type":"arena","layout":{"arena_scope":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"generation":1,"map_len":131072,"slot_capacity":16384,"slots":8}}
{"id":1,"outcome":"executed","generation":1}
```

The numbers above illustrate a layout, not a portable page-size assumption.
Slot N starts at `N * slot_capacity` in the payload mapping. Rust retains the
writer export until unmap acknowledgement or verified helper exit. Before resize,
it sends `arena_release {generation}`; the helper destroys its contexts and
mapping before acknowledging that generation. Rust then releases the old export,
reconfigures, and exports the replacement. A capacity pause uses
`advance_reconfiguration` on later scheduler turns, so an old consumer lease or
mapping can retire without restarting the transition. This also prevents a
writer export from permanently pinning a paused allocation.

For each snapshot Rust reserves one exclusive slot and sends:

```json
{"id":3,"type":"draw","slot":0,"generation":2,"width":800,"height":600,"stride":3200}
```

The helper rejects stale generations, out-of-range slots, invalid dimensions,
stride other than `width * 4`, payloads beyond the slot capacity or 64 MiB cap,
and dimensions inconsistent with the current presentation scale. It draws native
little-endian premultiplied BGRA directly into that slot, then replies:

```json
{"id":3,"outcome":"executed","generation":2,"slot":0,"frame":{"format":"bgra8","width":800,"height":600,"stride":3200,"len":1920000},"capture":{"published":true,"snapshot_ns":2100000,"publish_ns":900000}}
```

Rust verifies command ID, slot, allocation generation and the entire header before
commit. An unchanged draw carries `capture.published=false` and no frame header;
Rust abandons it. Hidden, loading and discarded attempts have no capture report
and also abandon their slot. A timeout, invalid reply, helper death or cancelled
reservation terminates and reaps the helper before releasing the reservation.
An acknowledgement followed by EOF before completion cannot publish a slot.
No pixel fallback exists; output tag 1 is retired and rejected.

The Swift helper sets `snapshotWidth` to
`logical_width * scale / window_backing_scale` and draws WebKit's original image
representation 1:1. That preserves odd fractional sizes which screen-scaled
`NSImage` CGImage extraction can round away. Each mapped slot reuses its CGContext;
the snapshot's RGB colour space is determined once per allocation. No channel
swizzle, intermediate pixel `Data`, Rust pixel vector or socket pixel write remains
in the helper path. Small command/ack JSON and WebKit snapshot objects still
allocate. Input geometry stays in the fixed logical viewport.

## Capture policy and reports

Rust requests one capture at a time, using the ordinary command IDs and acks.
A successful capture ack adds an optional report without changing its outcome:

```json
{"id":4,"outcome":"executed","generation":2,"slot":1,"capture":{"published":false,"snapshot_ns":2100000,"publish_ns":0}}
```

`published` means the helper completed a changed draw. Snapshot time measures
WebKit API latency; publish time measures drawing and hashing through completion
of the slot write. It excludes the small JSON acknowledgement and Rust commit.
Skipped frames report zero publish time. Hidden/loading attempts have no report.
`--stats` reports completed snapshots, Jackstay publications, unchanged skips,
the mean times and `copies_per_frame=1`. The copy count describes the explicit
full-frame destination writes in this path; it excludes WebKit internals and
hashing reads. Orderly shutdown allows up to one second to complete a pending
draw, then acknowledges unmap while keeping the helper alive for native input
cleanup. Helper termination follows toolkit shutdown.

The helper compares an FNV-1a fingerprint of dimensions and native pixels with
its last changed draw. A match acknowledges unchanged and writes no frame header.
Rust accepts that report and abandons the slot; it performs no second pixel scan.
After one second without a changed frame or wake, captures back off from `--fps`
(default 30) to 2 fps. Deadlines run from snapshot completion, so the effective
frame rate is lower than `--fps` by snapshot and publish latency. The Rust loop
sleeps until the next capture or 250 ms watch/signal deadline; acks, page activity
and presentation callbacks interrupt its condition-variable wait immediately.
A changed frame restores full rate. Reload and presentation
commands wake capture; input, navigation and presentation commands also report a wake; arena setup
and release do not.
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
again. `--frames=N` counts committed changed frames; a static page with N greater
than one can therefore remain running until content or presentation changes.

## Verification

`cargo test --workspace --locked` exercises fd export/mapping with fake Python
helpers, byte-exact consumer frames, unchanged-slot abandonment, resize and old
leases, capacity-paused retry, timeout, helper death (including death after ack),
and mismatched generation, slot, ID or header. Protocol tests cover split records,
retired pixel tags, out-of-order/late/duplicate acks, bounds, callback panic and
process reaping. CLI tests cover watch, final draws and ordered input cleanup.
`capture_policy` checks idle backoff, reload/page wakes, scale, hidden capture and
re-show. Fake helpers require `python3`, available on CI runners.

The ignored macOS tests exercise the production WebKit helper and native input,
including odd fractional dimensions and real bootstrap consumers. Run them
sequentially in a logged-in desktop after `scripts/build-helper.sh`.
