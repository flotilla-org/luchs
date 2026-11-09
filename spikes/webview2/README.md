# WebView2 headless spike

A throwaway Rust program that answers whether WebView2 can render offscreen, without a visible window and without taking the desktop's focus, well enough to back a Luchs renderer on Windows (#40). The approach follows GStreamer's `webview2src`. It was run on Beaufort (Windows 11 25H2, WebView2 Runtime 156.0.4314.58, AMD Radeon iGPU, display reached over RDP) on 2026-11-02.

It is a standalone crate, not a member of the luchs workspace, and it is not built by CI.

## How it renders

1. A message-only window (`HWND_MESSAGE`) hosts an `ICoreWebView2CompositionController`. Nothing is shown on screen.
2. A Windows.UI.Composition `ContainerVisual` is the controller's `RootVisualTarget`.
3. `GraphicsCaptureItem::CreateFromVisual` captures that visual into a `Direct3D11CaptureFramePool`. Each frame arrives as a BGRA `ID3D11Texture2D`.
4. Input goes through the DevTools Protocol (`Input.dispatchKeyEvent`, `Input.insertText`, `Input.dispatchMouseEvent`). The page sees these events as trusted.

## Findings

From `results/results.txt` (the full run) and `results/results_followup*.txt` (the follow-up run):

- **Frames:** these arrive offscreen at the page's size: 800×600 to start with, 1024×768 after a resize and a frame pool `Recreate`. The capture border and the cursor can both be turned off. `results/*.png` are captured frames.
- **Cadence:**
  - **Rate:** frames arrive at the compositor's rate, 32 fps on Beaufort's RDP display, with gaps of about 31 ms.
  - **Idle:** a static page produces almost no frames.
  - **Throttling:** `MinUpdateInterval` caps the rate (33 ms gave 16 fps and 100 ms gave 8 fps).
  - **Unthrottled page:** `--disable-gpu-vsync --disable-frame-rate-limit` lets the page's `requestAnimationFrame` run unthrottled, but capture still arrives at 32 fps.
- **Keyboard via CDP:**
  - Typing, editing keys, Ctrl+A, Tab and `insertText` (including `é😀`) all work, and every event is trusted.
  - The page's `document.hasFocus()` becomes true after the first key, without the OS foreground window changing.
- **Mouse via CDP:** clicks and the wheel are trusted, and the OS foreground window doesn't change.
- **Two ways to lose the foreground:**
  - The composition controller's `SendMouseInput` with button events makes the spike the foreground window (`results.txt`, Q4).
  - `controller.MoveFocus(PROGRAMMATIC)` does too (follow-up run, last line).
  - A renderer should use CDP for buttons and focus, and keep `SendMouseInput` for moves only.
- **Cursor:**
  - `CursorChanged` reports the system cursor id for `SendMouseInput` moves, after about 10–15 ms and only once a second move lands on the element.
  - CDP mouse moves don't update it.
  - The CSS `cursor` value can be read from the page instead.
- **Visibility:** `IsVisible = FALSE` stops frames, sets `visibilityState` to `"hidden"` and pauses `requestAnimationFrame`. Setting it back to `TRUE` resumes them.

## Cost compared with CEF

Each frame costs one compositor frame of latency and a GPU copy out of the frame pool, before any copy into a Jackstay slot. CEF's `OnAcceleratedPaint` instead lends its own texture, which saves the compositor hop.

## Build and run

`build.cmd` sets the MSVC linker explicitly, because on Beaufort Git's `link.exe` comes first on `PATH`. Then:

```
run.cmd            full run, Q1–Q7, saving frames here
run.cmd followup   follow-up run: focus, CDP input, cursor ids, visibility, MoveFocus
```

The follow-up run reads `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`; `results_followup_novsync.txt` was run with `--disable-gpu-vsync --disable-frame-rate-limit`. The spike writes frames and its WebView2 user data folder (`udf/`) into the current directory.

Both runs briefly take the foreground near the end: the full run in Q4 and the follow-up run when it calls `MoveFocus`.
