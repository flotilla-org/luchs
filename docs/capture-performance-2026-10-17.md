# Capture cadence and pixel comparison, 17 October 2026

The previous policy waited another frame interval after each snapshot completed.
At 30 fps, a 20 ms capture therefore took about 53 ms per cycle. Animation
notifications could bypass that delay, making cadence depend on the page.

Capture admission now advances a deadline. Small timer delays retain its phase;
an overrun rebases the next capture without accumulating catch-up work. Native
wakes coalesce within that cadence. One snapshot remains in flight, and the
existing one-second active tail, 500 ms idle probe and hidden pause remain.

Scheduling no longer injects an activity script, observes DOM mutations or
enumerates document animations. Native commands and navigation wake capture;
autonomous page changes are discovered by the idle probe, then changed pixels
keep capture active. This can take up to 500 ms plus snapshot time after an idle
page starts changing; brief autonomous changes can fall between idle probes.
Existing caret and cursor/scroll affordance scripts remain.

Exact byte comparison replaces the serial FNV hash. It keeps one private previous
image, bounded to 64 MiB, and uses libc comparison/copy operations. Changed frames
add a copy into that image; unchanged frames scan without copying. This trades
bounded memory and a fast copy for the hash's serial dependency and removes hash
collisions. An arena pointer is never retained after its acknowledgement.

Frame timestamps now retain capture-request time in Jackstay's host clock,
including helper queueing. They do not identify WebKit's internal paint time.
Non-helper fixture publication retains its zero timestamp default.

## Validation

Apple M4, 24 GiB, macOS 26.6; Rust and Swift release builds. Workloads ran on a
shared interactive machine. CPU samples were separate from throughput runs.

- `cargo test --workspace --locked`: 70 passing tests, with live/child-only tests
  ignored by the default suite. Includes cadence, timer drift, overruns, coalesced
  wakes, idle, visibility, helper failure, arena ownership and timestamp retention.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` and
  `cargo fmt --all --check`: passed.
- `scripts/test-helper.sh`: passed socket rights, recovery, exact pixel changes,
  dimension/invalidation handling, wide images, affordances and writer imports.
- `cargo test --locked --test live_macos -- --ignored --test-threads=1`: eight
  passed, including native input, reload/watch, hidden/show, fractional scale,
  mapping replacement, socket shutdown and slow navigation. The input test first
  timed out during a concurrent build; the subsequent full live run passed.
- The new idle canvas test uses the real CLI, Swift helper and Jackstay consumer.
  After 2.5 seconds of static pixels it starts canvas animation without DOM
  mutations or native input. Delivery resumes and returns to active cadence.
  It passed again after the final timer-phase adjustment.

Separate Wheelhouse changes preserve BGRA and transfer the private mailbox image
instead of copying it on the GUI thread. They also use the existing shared Metal
texture path for CPU images. The following end-to-end CPU-consumer measurements
include those session changes, but exclude host texture upload/presentation:

| Workload at scale 2 | Before fps | First patch fps | Final deadline phase fps |
| --- | ---: | ---: | ---: |
| Canvas, 800 × 600 logical | 18.625 | 27.625 | 24.25 |
| Canvas, 1474 × 1020 logical | 10.875 | 22.0 | 12.5 |
| CSS transforms, 800 × 600 | 29.875 | 27.375 | 28.75 |
| Local 720p60 video, 800 × 600 | 16.375 | 27.125 | 29.5 |
| Basic WebGL2, 800 × 600 | 15.875 | 27.0 | 29.875 |

Each measurement has a two-second warmup and eight-second sample. These are
observations, not isolated capacity claims. In the final large-canvas run, the
page's own rAF fell to 7–22 fps and p95 image age reached 259 ms. Other runs had
better throughput and latency. No broad claim of solved large-page pacing follows
from these results. Video/WebGL final median delivery intervals were about 33 ms;
their p95 remained about 44–45 ms. The default 30 fps ceiling remains unchanged.

Machine-local artifacts, not checked into this repository, are in
`wheelhouse/local/diagnostics/web-performance/implementation`: `final.json` is the
before/first-patch comparison; `paced.json` is the final deadline run. Scripts,
fixtures, console logs and per-run outputs are alongside them.

### Launch classification correction

The earlier throughput table does not establish a WebKit capacity limit. A later
controlled comparison on an unlocked, mostly idle M4 machine used the same frozen
`7e01d51` executable and helper, a page encoding its frame identity in pixels, and
an Interactive consumer. Changing only the source's launchd `ProcessType` produced:

| Physical image | Standard images/sec, two trials | Interactive images/sec, two trials |
| --- | ---: | ---: |
| 800 × 600 | 18.250 / 20.375 | 50.000 / 50.000 |
| 2948 × 2040 | 32.375 / 32.875 | 50.125 / 50.000 |

The display's primary clock was 50 Hz. Every Interactive run received every page
frame without duplicates over its eight-second sample. The 50.125 result reflects
the finite counting window. Large snapshots still averaged about 10.6 ms, with
another 2.3 ms for helper drawing/publication. Delivery p95 gaps were 34–36 ms;
this is not perfect pacing or a measurement of Wheelhouse presentation.

The historical subprocess launches did not record their task roles, so this does
not prove their exact cause. Future engine comparisons must establish interactive
classification, visible animation, and matched display/workload conditions first.
The repeated comparison reversed launch order and excluded earlier screen-locked
runs. Local evidence is under
`wheelhouse/local/diagnostics/web-performance/launch-classification/unlocked-{1,2}`.

No reproduction established that removing animation enumeration fixes the
observed YouTube WebKit crash. Its stack involved `getAnimations`; this change
removes our use of that API but cannot prevent a page from calling it.

Display-clock coupling, higher-refresh presentation measurements and alternative
browser engines remain separate work. Another connector, such as CEF, can coexist
with this WebKit implementation. GPU transport does not itself remove WebKit's
bitmap snapshot/readback boundary.
