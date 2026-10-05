import Cocoa
import WebKit
import Carbon

private let defaultWidth = 800
private let defaultHeight = 600
private let defaultFrameCount = 1
private let defaultFps = 15

private func fail(_ message: String) -> Never {
    fputs("luchs-webview-capture: \(message)\n", stderr)
    exit(1)
}

// Page console output, page errors and navigations, one line each, in the
// file LUCHS_CONSOLE_LOG names (default /tmp/luchs-console-<pid>.log). The
// helper's socket carries commands and acknowledgements and its stderr is the launcher's, so this
// is the one place a page can be debugged from.
private let consoleLogPath: String = ProcessInfo.processInfo.environment["LUCHS_CONSOLE_LOG"]
    ?? "/tmp/luchs-console-\(getpid()).log"
private let consoleLogHandle: FileHandle? = {
    FileManager.default.createFile(atPath: consoleLogPath, contents: nil)
    return FileHandle(forWritingAtPath: consoleLogPath)
}()
private let consoleLogQueue = DispatchQueue(label: "luchs.console-log")
private let consoleLogStart = Date()

private func debugLog(_ line: String) {
    guard let handle = consoleLogHandle else { return }
    let stamp = String(format: "%8.3f", Date().timeIntervalSince(consoleLogStart))
    let text = "\(stamp) \(line)\n"
    consoleLogQueue.async {
        handle.seekToEndOfFile()
        handle.write(Data(text.utf8))
    }
}

private let consoleForwarderSource = """
(() => {
  const describe = (value) => {
    if (typeof value === "string") return value;
    if (value instanceof Error) return value.stack || value.message;
    try { return JSON.stringify(value); } catch (_) { return String(value); }
  };
  const post = (level, args) => {
    try {
      const text = args.map(describe).join(" ").slice(0, 4000);
      window.webkit.messageHandlers.luchsConsole.postMessage({ level, text });
    } catch (_) {}
  };
  for (const level of ["log", "info", "warn", "error", "debug"]) {
    const original = typeof console[level] === "function" ? console[level].bind(console) : null;
    console[level] = (...args) => { post(level, args); if (original) original(...args); };
  }
  window.addEventListener("error", (e) => post("error", [`${e.message} (${e.filename}:${e.lineno}:${e.colno})`]));
  window.addEventListener("unhandledrejection", (e) => post("error", ["unhandled rejection:", e.reason]));
  // Liveness of the page clock: an offscreen view may never run
  // requestAnimationFrame, and a page that awaits one hangs.
  let frame = false;
  requestAnimationFrame(() => { frame = true; post("debug", ["requestAnimationFrame fired"]); });
  setTimeout(() => post("debug", [`after 2s: visibility ${document.visibilityState}, hasFocus ${document.hasFocus()}, requestAnimationFrame ${frame ? "ran" : "never ran"}`]), 2000);
})();
"""

// A caret for the frame's focused text field. WebKit draws its own only
// while its window is key, which this transparent window never is, so the
// page gets one drawn from the selection; it follows focus, input and
// scrolling from page events, iframes included.
private let caretScriptSource = """
(() => {
  try {
  window.__luchsEnsureCaret = () => {
    if (window.__luchsCaretInstalled) return;
    window.__luchsCaretInstalled = true;
    const style = document.createElement("style");
    style.textContent = `
      #luchs-synthetic-caret {
        position: fixed;
        display: none;
        width: 2px;
        background: #f6c343;
        pointer-events: none;
        z-index: 2147483647;
        animation: luchs-caret-blink 1s steps(1, end) infinite;
      }
      @keyframes luchs-caret-blink {
        0%, 49% { opacity: 1; }
        50%, 100% { opacity: 0; }
      }
    `;
    document.head.appendChild(style);

    const caret = document.createElement("div");
    caret.id = "luchs-synthetic-caret";
    document.documentElement.appendChild(caret);

    const textLikeInput = (element) => {
      if (element instanceof HTMLTextAreaElement) return true;
      if (!(element instanceof HTMLInputElement)) return false;
      const type = (element.type || "text").toLowerCase();
      return ["text", "search", "url", "tel", "email", "password"].includes(type);
    };
    const numericStyle = (computed, name) => {
      const value = Number.parseFloat(computed[name]);
      return Number.isFinite(value) ? value : 0;
    };
    const update = () => {
      const active = document.activeElement;
      if (!active || !textLikeInput(active) || active.disabled || active.readOnly) {
        caret.style.display = "none";
        return;
      }
      const rect = active.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) {
        caret.style.display = "none";
        return;
      }
      const computed = getComputedStyle(active);
      const canvas = window.__luchsCaretCanvas || (window.__luchsCaretCanvas = document.createElement("canvas"));
      const context = canvas.getContext("2d");
      context.font = computed.font || `${computed.fontSize} ${computed.fontFamily}`;

      const selectionStart = typeof active.selectionStart === "number" ? active.selectionStart : active.value.length;
      const prefix = active.value.slice(0, selectionStart);
      const borderLeft = numericStyle(computed, "borderLeftWidth");
      const borderRight = numericStyle(computed, "borderRightWidth");
      const borderTop = numericStyle(computed, "borderTopWidth");
      const borderBottom = numericStyle(computed, "borderBottomWidth");
      const paddingLeft = numericStyle(computed, "paddingLeft");
      const paddingTop = numericStyle(computed, "paddingTop");
      const paddingBottom = numericStyle(computed, "paddingBottom");
      const fontSize = numericStyle(computed, "fontSize") || 16;
      const lineHeightValue = Number.parseFloat(computed.lineHeight);
      const lineHeight = Number.isFinite(lineHeightValue) ? lineHeightValue : fontSize * 1.2;
      const contentHeight = Math.max(1, rect.height - borderTop - borderBottom - paddingTop - paddingBottom);
      const caretHeight = Math.max(8, Math.min(lineHeight, contentHeight));
      const measured = context.measureText(prefix).width;
      const minimumX = rect.left + borderLeft + paddingLeft;
      const maximumX = Math.max(minimumX, rect.right - borderRight - 2);
      const x = Math.min(maximumX, Math.max(minimumX, minimumX + measured - active.scrollLeft));
      const y = active instanceof HTMLTextAreaElement
        ? rect.top + borderTop + paddingTop - active.scrollTop
        : rect.top + borderTop + paddingTop + Math.max(0, (contentHeight - caretHeight) / 2);

      caret.style.display = "block";
      caret.style.left = `${Math.round(x)}px`;
      caret.style.top = `${Math.round(y)}px`;
      caret.style.height = `${Math.round(caretHeight)}px`;
    };
    window.__luchsUpdateCaret = update;
    let queued = false;
    const schedule = () => {
      if (queued) return;
      queued = true;
      requestAnimationFrame(() => { queued = false; update(); });
    };
    for (const name of ["focusin", "focusout", "input", "keydown", "keyup", "mousedown", "mouseup"]) {
      document.addEventListener(name, schedule, true);
    }
    document.addEventListener("selectionchange", schedule, true);
    window.addEventListener("scroll", schedule, true);
    window.addEventListener("resize", schedule, true);
    update();
  };
  window.__luchsEnsureCaret();
  } catch (error) { console.error("luchs caret script failed:", error && (error.stack || error.message || error)); }
})();
"""

// The socket writer is serial and runs off WebKit's main thread. A command
// reader waits for its acknowledgement to drain before admitting another.
private let transport: FileHandle = {
    guard let value = ProcessInfo.processInfo.environment["LUCHS_HELPER_FD"],
          let fd = Int32(value), fd >= 0 else { fail("LUCHS_HELPER_FD must name an inherited socket") }
    guard fcntl(fd, F_SETFD, FD_CLOEXEC) == 0 else { fail("could not protect inherited socket") }
    var noSigpipe: Int32 = 1
    guard setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSigpipe, socklen_t(MemoryLayout<Int32>.size)) == 0 else {
        fail("could not configure helper socket")
    }
    return FileHandle(fileDescriptor: fd, closeOnDealloc: true)
}()
private let writer = DispatchQueue(label: "luchs.socket-writer")
private let commandDrained = DispatchSemaphore(value: 0)

private func writeBytes(_ pointer: UnsafeRawPointer, _ count: Int) {
    var offset = 0
    while offset < count {
        let n = Darwin.write(transport.fileDescriptor, pointer.advanced(by: offset), count - offset)
        if n < 0 && errno == EINTR { continue }
        if n <= 0 { fail("socket write failed") }
        offset += n
    }
}
private func writeData(_ data: Data) {
    data.withUnsafeBytes { if let base = $0.baseAddress { writeBytes(base, $0.count) } }
}

// Mutations and native edit/selection events wake capture. Probe animation
// activity only while something is running; static pages have no rAF loop.
private let activityScriptSource = """
(() => {
  let last = -Infinity, probing = false;
  const changed = () => {
    const now = performance.now();
    if (now - last < 30) return;
    last = now;
    window.webkit.messageHandlers.luchsActivity.postMessage({capture_changed: true});
  };
  const animations = () => {
    if (document.getAnimations().some(a => a.playState === "running")) {
      changed();
      requestAnimationFrame(animations);
    } else { probing = false; }
  };
  const probe = () => {
    if (!probing) { probing = true; requestAnimationFrame(animations); }
  };
  new MutationObserver(() => { changed(); probe(); }).observe(document, {subtree:true, childList:true, attributes:true, characterData:true});
  for (const name of ["input", "change", "selectionchange", "focusin", "focusout", "scroll", "resize"]) {
    window.addEventListener(name, changed, true);
  }
  for (const name of ["animationstart", "transitionrun"]) {
    window.addEventListener(name, () => { changed(); probe(); }, true);
  }
  probe();
})();
"""

private let maxControlBytes = 128 * 1024
private let maxFrameBytes = 64 * 1024 * 1024

private let activityRecord: Data = {
    let body = Data("{\"capture_changed\":true}".utf8)
    var record = littleEndian(UInt32(body.count + 1))
    record.append(3)
    record.append(body)
    return record
}()

private struct WriterLayout: Decodable {
    let generation: UInt64
    let map_len: Int
    let slot_capacity: Int
    let slots: UInt32
}

// Contains payload bytes only. The helper does not read arena bookkeeping.
private final class WriterMapping {
    let layout: WriterLayout
    let base: UnsafeMutableRawPointer
    // Each arena command replaces this whole mapping, so cached slot contexts
    // cannot outlive their allocation generation or refer to retired memory.
    private var contexts: [UInt32: (width: Int, height: Int, stride: Int, graphics: NSGraphicsContext)] = [:]
    private var colorSpace: CGColorSpace?
    init(_ layout: WriterLayout, fd: Int32) {
        defer { close(fd) }
        var info = stat()
        guard layout.generation > 0, layout.slots > 0, layout.slot_capacity > 0,
              layout.map_len > 0, layout.slot_capacity <= layout.map_len / Int(layout.slots),
              fstat(fd, &info) == 0, info.st_size >= layout.map_len,
              let address = mmap(nil, layout.map_len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0),
              address != MAP_FAILED else { fail("invalid arena writer mapping") }
        self.layout = layout
        self.base = address
    }
    deinit { contexts.removeAll(); munmap(base, layout.map_len) }
    func slot(_ command: HelperCommand) -> UnsafeMutableRawPointer? {
        guard command.generation == layout.generation, let slot = command.slot, slot < layout.slots,
              let width = command.width, let height = command.height, let stride = command.stride,
              width > 0, height > 0, width <= maxFrameBytes / 4,
              stride == width * 4, height <= maxFrameBytes / stride,
              stride * height <= layout.slot_capacity else { return nil }
        return base.advanced(by: Int(slot) * layout.slot_capacity)
    }
    func graphicsContext(_ command: HelperCommand, image: NSImage) -> NSGraphicsContext? {
        guard let pointer = slot(command), let index = command.slot,
              let width = command.width, let height = command.height, let stride = command.stride else { return nil }
        if let context = contexts[index] {
            guard context.width == width, context.height == height, context.stride == stride else { return nil }
            return context.graphics
        }
        if colorSpace == nil { colorSpace = image.cgImage(forProposedRect: nil, context: nil, hints: nil)?.colorSpace }
        guard let space = colorSpace,
              let context = CGContext(data: pointer, width: width, height: height,
                  bitsPerComponent: 8, bytesPerRow: stride, space: space,
                  bitmapInfo: CGBitmapInfo.byteOrder32Little.rawValue | CGImageAlphaInfo.premultipliedFirst.rawValue)
        else { return nil }
        let result = NSGraphicsContext(cgContext: context, flipped: false)
        contexts[index] = (width, height, stride, result)
        return result
    }

}

private struct HelperCommand: Decodable {
    let layout: WriterLayout?
    let slot: UInt32?, generation: UInt64?
    let width: Int?, height: Int?, stride: Int?
    let id: UInt64
    let type: String
    let scale: Double?
    let visible: Bool?
    let x: Double?, y: Double?, dx: Double?, dy: Double?
    let pointDx: Int64?, pointDy: Int64?
    let button: Int?, press: UInt64?, keyCode: UInt16?, modifiers: UInt32?
    let logical: String?, text: String?, scope: String?
    let cooperative: Bool?, isRepeat: Bool?
    let url: String?
    let axis: String?
    let position: Double?
    let step: String?
    let direction: String?
    enum CodingKeys: String, CodingKey {
        case layout, slot, generation, width, height, stride
        case id, type, scale, visible, x, y, dx, dy, button, press, modifiers, logical, text, scope, cooperative
        case url, axis, position, step, direction
        case pointDx = "point_dx", pointDy = "point_dy", keyCode = "key_code", isRepeat = "repeat"
    }
}

private func littleEndian(_ value: UInt32) -> Data {
    var value = value.littleEndian
    return withUnsafeBytes(of: &value) { Data($0) }
}

private func readExactly(_ count: Int, allowEOF: Bool = false, descriptor: inout Int32?) -> Data? {
    var result = Data()
    while result.count < count {
        var chunk = Data(count: count - result.count)
        var fd: Int32 = -1
        let n = chunk.withUnsafeMutableBytes { bytes in
            luchs_recv_rights(transport.fileDescriptor, bytes.baseAddress, bytes.count, &fd)
        }
        if n < 0 && errno == EINTR { continue }
        if n < 0 { fail("command recvmsg failed") }
        if fd >= 0 {
            // macOS has no MSG_CMSG_CLOEXEC. The helper launches no child
            // processes, so no fork/exec can race this fallback in our code.
            guard descriptor == nil, fcntl(fd, F_SETFD, FD_CLOEXEC) == 0 else {
                close(fd)
                fail("unexpected command descriptors")
            }
            descriptor = fd
        }
        if n == 0 {
            if allowEOF && result.isEmpty && descriptor == nil { return nil }
            fail("truncated command record")
        }
        chunk.count = n
        result.append(chunk)
    }
    return result
}

private func emitAck(_ id: UInt64, outcome: String, detail: String? = nil, capture: [String: Any]? = nil, generation: UInt64? = nil, slot: UInt32? = nil, frame: [String: Any]? = nil) {
    precondition(Thread.isMainThread)
    var object: [String: Any] = ["id": id, "outcome": outcome]
    if let detail { object["detail"] = detail }
    if let capture { object["capture"] = capture }
    if let generation { object["generation"] = generation }
    if let slot { object["slot"] = slot }
    if let frame { object["frame"] = frame }
    guard let json = try? JSONSerialization.data(withJSONObject: object), json.count + 1 <= maxControlBytes else {
        fail("could not encode acknowledgement")
    }
    var record = littleEndian(UInt32(json.count + 1))
    record.append(2)
    record.append(json)
    let bytes = record
    writer.async {
        writeData(bytes)
        commandDrained.signal()
    }
}

private func emitDrawAck(_ command: HelperCommand, outcome: String, detail: String? = nil, capture: [String: Any]? = nil, frame: [String: Any]? = nil) {
    emitAck(command.id, outcome: outcome, detail: detail, capture: capture,
            generation: command.generation, slot: command.slot, frame: frame)
}

private func emitState(_ object: [String: Any]) {
    guard let json = try? JSONSerialization.data(withJSONObject: object), json.count + 1 <= maxControlBytes else {
        debugLog("ignoring oversized or invalid page state")
        return
    }
    var record = littleEndian(UInt32(json.count + 1))
    record.append(3)
    record.append(json)
    let bytes = record
    writer.async { writeData(bytes) }
}

private final class CaptureController: NSObject, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler {
    // A local file, or an http(s) page. The default website data store is
    // persistent for this binary (under ~/Library/WebKit), so a login made in
    // one run is still there in the next.
    private let pageURL: URL
    private let width: Int
    private let height: Int
    private var window: NSWindow?
    private var webView: WKWebView?
    // Windows the page opened (OAuth sign-in, target=_blank), newest last.
    // The capture and the input follow the newest one while it is open, so
    // a sign-in popup is what the panel shows and types into; when the page
    // closes it, the main view is back. The popup keeps its opener, so a
    // flow that posts its result back to the opener completes.
    private var popups: [WKWebView] = []
    private var activeView: WKWebView? { popups.last ?? webView }
    private var observations: [NSKeyValueObservation] = []
    private var navigationFinished = false
    private var loaded = false
    private var everLoadedMain = false
    private let frameCount: Int
    private let snapshotConfiguration = WKSnapshotConfiguration()
    private var configuredScale = 0.0
    private var configuredBacking = 0.0
    private var scale = 1.0
    private var visible = true
    private var arena: WriterMapping?
    private var fingerprint: UInt64?
    private var emittedFrames = 0
    private var activityPending = false
    private var snapshotRecovery = SnapshotRecovery()

    init(pageURL: URL, width: Int, height: Int, frameCount: Int) {
        self.frameCount = frameCount
        self.pageURL = pageURL
        self.width = width
        self.height = height

    }

    func run() -> Never {
        NSApplication.shared.setActivationPolicy(.prohibited)

        let rect = NSRect(x: -20000, y: -20000, width: width, height: height)
        let configuration = WKWebViewConfiguration()
        // Sign-in providers refuse a bare WebKit agent as an embedded view;
        // the Safari tokens make this the browser it effectively is.
        configuration.applicationNameForUserAgent = "Version/17.4 Safari/605.1.15"
        configuration.userContentController.addUserScript(
            WKUserScript(source: consoleForwarderSource, injectionTime: .atDocumentStart, forMainFrameOnly: false))
        configuration.userContentController.add(self, name: "luchsConsole")
        configuration.userContentController.add(self, name: "luchsActivity")
        configuration.userContentController.add(self, name: "luchsState")
        configuration.userContentController.addUserScript(
            WKUserScript(source: pageAffordancesScript, injectionTime: .atDocumentEnd, forMainFrameOnly: true))
        configuration.userContentController.addUserScript(
            WKUserScript(source: activityScriptSource, injectionTime: .atDocumentEnd, forMainFrameOnly: false))
        configuration.userContentController.addUserScript(
            WKUserScript(source: caretScriptSource, injectionTime: .atDocumentEnd, forMainFrameOnly: false))
        debugLog("luchs-webview-capture \(width)x\(height) page \(pageURL.absoluteString)")
        let view = WKWebView(frame: NSRect(x: 0, y: 0, width: width, height: height), configuration: configuration)
        view.navigationDelegate = self
        view.uiDelegate = self
        let container = NSView(frame: rect)
        container.addSubview(view)

        let captureWindow = CaptureInputWindow(
            contentRect: rect,
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        captureWindow.contentView = container
        // WebKit treats an off-screen or occluded window as a hidden page:
        // requestAnimationFrame stops, timers slow, and a page that awaits a
        // frame (a sign-in form after submit, say) hangs. So the window is on
        // screen, fully transparent, ignoring the mouse, on every Space and
        // above fullscreen apps, so it is never occluded and never seen. The
        // capture reads the view through takeSnapshot, not the screen.
        captureWindow.alphaValue = 0.0
        captureWindow.acceptsMouseMovedEvents = true
        captureWindow.ignoresMouseEvents = true
        captureWindow.hasShadow = false
        captureWindow.level = .screenSaver
        captureWindow.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
        // The primary screen's origin, so that window coordinates are screen
        // coordinates: a scroll event built from a CGEvent carries only those.
        if let screen = NSScreen.screens.first {
            captureWindow.setFrameOrigin(NSPoint(x: screen.frame.minX, y: screen.frame.minY))
        }
        captureWindow.orderFront(nil)

        self.webView = view
        self.window = captureWindow
        observePage(view)

        startInputReader()
        if pageURL.isFileURL {
            view.loadFileURL(pageURL, allowingReadAccessTo: pageURL.deletingLastPathComponent())
        } else {
            view.load(URLRequest(url: pageURL))
        }
        if frameCount > 0 {
            DispatchQueue.main.asyncAfter(deadline: .now() + 5.0) { [weak self] in
                if self?.loaded == false {
                    fail("timed out loading \(self?.pageURL.absoluteString ?? "html")")
                }
            }
        }
        NSApplication.shared.run()
        fail("application run loop exited unexpectedly")
    }

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let body = message.body as? [String: Any] else { return }
        if message.name == "luchsState" {
            guard message.frameInfo.isMainFrame, message.webView === activeView,
                  let domain = body["domain"] as? String, ["cursor", "scroll"].contains(domain) else { return }
            emitState(body)
            return
        }
        if message.name == "luchsActivity" {
            reportActivity()
            return
        }
        let level = body["level"] as? String ?? "log"
        let text = body["text"] as? String ?? ""
        debugLog("console.\(level) \(text)")
    }

    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
        debugLog("navigation start \(webView.url?.absoluteString ?? "?")")
        if webView === activeView {
            loaded = false
            resetDocumentState()
        }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        debugLog("navigation finish \(webView.url?.absoluteString ?? "?")")
        if webView === self.webView { everLoadedMain = true }
        if webView === activeView {
            loaded = true
            navigationFinished = true
            publishPageState()
        }
        reportActivity()
    }

    // window.open and target=_blank get a real second view (without a
    // delegate they return null and a sign-in flow fails on the spot). It
    // takes the panel's full size and sits above the main view; the
    // configuration WebKit hands over keeps the opener link and the data
    // store, so the popup shares the login.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        guard let container = window?.contentView else { return nil }
        let popup = WKWebView(frame: container.bounds, configuration: configuration)
        popup.navigationDelegate = self
        popup.uiDelegate = self
        container.addSubview(popup)
        popups.append(popup)
        loaded = false
        observePage(popup)
        debugLog("popup \(popups.count) opened for \(navigationAction.request.url?.absoluteString ?? "?")")
        return popup
    }

    func webViewDidClose(_ webView: WKWebView) {
        guard let index = popups.firstIndex(of: webView) else { return }
        popups.remove(at: index)
        webView.removeFromSuperview()
        if let view = activeView {
            loaded = !view.isLoading
            observePage(view)
            view.evaluateJavaScript("window.__luchsPublishPageState && window.__luchsPublishPageState()", completionHandler: nil)
        }
        debugLog("popup \(index + 1) closed by the page; \(popups.isEmpty ? "main view" : "popup \(popups.count)") is active")
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        navigationFailed(webView, error)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        navigationFailed(webView, error)
    }

    // The first page failing to load ends the helper. Later failures do not:
    // a popup's, or a load cancelled by a redirect or by the page itself
    // (NSURLErrorCancelled), are the page's business and are only logged.
    private func navigationFailed(_ webView: WKWebView, _ error: Error) {
        let cancelled = (error as NSError).domain == NSURLErrorDomain && (error as NSError).code == NSURLErrorCancelled
        let view = webView === self.webView ? "main view" : "popup"
        debugLog("navigation failed in \(view): \(error.localizedDescription)")
        if webView === self.webView && !everLoadedMain && !cancelled {
            fail("navigation failed: \(error.localizedDescription)")
        }
        if webView === activeView && everLoadedMain && !cancelled {
            loaded = true
            reportActivity()
        }
    }

    private func observePage(_ view: WKWebView) {
        // One observer set owns the active view. Replacement invalidates the
        // previous set; popup closure reattaches it to the newly active view.
        observations = [
            view.observe(\.title, options: [.new]) { [weak self] _, _ in self?.publishPageState() },
            view.observe(\.url, options: [.new]) { [weak self] _, _ in self?.publishPageState() },
            view.observe(\.canGoBack, options: [.new]) { [weak self] _, _ in self?.publishPageState() },
            view.observe(\.canGoForward, options: [.new]) { [weak self] _, _ in self?.publishPageState() },
            view.observe(\.isLoading, options: [.new]) { [weak self] _, _ in self?.publishPageState() }
        ]
        publishPageState()
        resetDocumentState()
    }

    private func publishPageState() {
        guard let view = activeView else { return }
        // Advisory helper readiness: emittedFrames counts completed draws. Rust
        // additionally gates this on publication through the producer toolkit.
        // It stays true after the first completed navigation; later loads use
        // navigation.loading to describe progress without hiding the window.
        emitState(["domain": "window", "body": ["title": view.title as Any? ?? NSNull(),
            "requested_size": NSNull(), "ready": navigationFinished && emittedFrames > 0]])
        emitState(["domain": "navigation", "body": ["url": view.url?.absoluteString as Any? ?? NSNull(),
            "title": view.title as Any? ?? NSNull(), "can_go_back": view.canGoBack,
            "can_go_forward": view.canGoForward, "loading": view.isLoading, "capabilities": [:]]])
    }

    private func resetDocumentState() {
        emitState(["domain": "cursor", "body": ["shape": "default"]])
        let axis: [String: Any] = ["scrollable": false, "content_length": 0, "viewport_length": 0, "position": 0]
        emitState(["domain": "scroll", "body": ["x": axis, "y": axis, "capabilities": [:]]])
    }

    private func startInputReader() {
        DispatchQueue.global(qos: .userInteractive).async { [weak self] in
            // Only one command is queued at a time. Waiting for main-thread
            // handling and writer completion bounds memory under command floods.
            while true {
                var descriptor: Int32?
                guard let prefix = readExactly(4, allowEOF: true, descriptor: &descriptor) else { break }
                let size = prefix.enumerated().reduce(UInt32(0)) { value, byte in
                    value | (UInt32(byte.element) << (8 * byte.offset))
                }
                guard size > 0 && size <= UInt32(maxControlBytes) else {
                    fail("invalid command record length")
                }
                guard let data = readExactly(Int(size), descriptor: &descriptor),
                      let command = try? JSONDecoder().decode(HelperCommand.self, from: data),
                      !command.type.isEmpty else {
                    fail("invalid command JSON")
                }
                DispatchQueue.main.sync { [weak self] in
                    guard let self else { fail("command controller stopped") }
                    if command.type != "arena", let fd = descriptor {
                        close(fd)
                        fail("descriptor on non-arena command")
                    }
                    self.handleCommand(command, descriptor: descriptor)
                }
                commandDrained.wait()
            }
            writer.async { DispatchQueue.main.async { NSApplication.shared.terminate(nil) } }
        }
    }

    private func reportActivity() {
        // Coalesce page wakes while one state record is in flight.
        guard !activityPending else { return }
        activityPending = true
        writer.async { [weak self] in
            writeData(activityRecord)
            DispatchQueue.main.async { self?.activityPending = false }
        }
    }

    private func handleCommand(_ command: HelperCommand, descriptor: Int32?) {
        precondition(Thread.isMainThread)
        if command.type != "draw" && command.type != "arena" && command.type != "arena_release" { reportActivity() }
        switch command.type {
        case "arena":
            guard let layout = command.layout, let fd = descriptor else { fail("arena command needs one payload descriptor") }
            arena = WriterMapping(layout, fd: fd) // releases the previous mapping before ack
            fingerprint = nil
            emitAck(command.id, outcome: "executed", generation: layout.generation)
        case "arena_release":
            guard let generation = command.generation, generation == arena?.layout.generation else {
                emitAck(command.id, outcome: "failed", detail: "stale arena release")
                return
            }
            arena = nil
            emitAck(command.id, outcome: "executed", generation: generation)
        case "draw":
            guard let arena, arena.slot(command) != nil,
                  command.width == Int((Double(width) * scale).rounded()),
                  command.height == Int((Double(height) * scale).rounded()) else {
                emitDrawAck(command, outcome: "failed", detail: "stale generation or invalid draw layout")
                return
            }
            // Never park the command reader behind page loading. Navigation
            // completion wakes Rust; ping, reload and visibility remain usable.
            if !loaded { emitDrawAck(command, outcome: "executed") } else { capture(command) }
        case "presentation":
            guard let scale = command.scale, let visible = command.visible,
                  scale.isFinite, scale > 0 else {
                emitAck(command.id, outcome: "failed", detail: "invalid presentation")
                return
            }
            let pw = (Double(width) * scale).rounded()
            let ph = (Double(height) * scale).rounded()
            guard pw >= 1 && ph >= 1 && pw * ph * 4 <= Double(maxFrameBytes) else {
                emitAck(command.id, outcome: "failed", detail: "pixel size exceeds capture limit")
                return
            }
            if self.scale != scale || (!self.visible && visible) { fingerprint = nil }
            self.scale = scale
            self.visible = visible
            emitAck(command.id, outcome: "executed")
        case "ping":
            emitAck(command.id, outcome: "executed")
        case "navigation.rejected":
            debugLog("rejected navigation.load: \(command.url ?? "missing URL")")
            emitAck(command.id, outcome: "executed")
        case "navigation.back", "navigation.forward", "navigation.reload", "navigation.stop", "navigation.load":
            guard let view = activeView else {
                emitAck(command.id, outcome: "failed", detail: "active view unavailable")
                return
            }
            switch command.type {
            case "navigation.back": view.goBack()
            case "navigation.forward": view.goForward()
            case "navigation.reload": view.reloadFromOrigin()
            case "navigation.stop": view.stopLoading()
            default:
                guard let value = command.url, let url = allowedNavigationURL(value, startupPage: pageURL) else {
                    debugLog("rejected navigation.load: \(command.url ?? "missing URL")")
                    emitAck(command.id, outcome: "executed")
                    return
                }
                if url.isFileURL {
                    view.loadFileURL(url, allowingReadAccessTo: pageURL.deletingLastPathComponent())
                } else { view.load(URLRequest(url: url)) }
            }
            emitAck(command.id, outcome: "executed")
        case "scroll.set_position", "scroll.scroll_by_step":
            guard let view = activeView, let axis = command.axis, ["x", "y"].contains(axis),
                  command.type != "scroll.set_position" || command.position?.isFinite == true,
                  command.type != "scroll.scroll_by_step" ||
                    (["small", "large"].contains(command.step ?? "") && ["increment", "decrement"].contains(command.direction ?? "")) else {
                emitAck(command.id, outcome: "failed", detail: "invalid scroll command")
                return
            }
            var object: [String: Any] = ["type": command.type, "axis": axis]
            if let position = command.position { object["position"] = position }
            if let step = command.step { object["step"] = step }
            if let direction = command.direction { object["direction"] = direction }
            guard let data = try? JSONSerialization.data(withJSONObject: object), let json = String(data: data, encoding: .utf8) else {
                emitAck(command.id, outcome: "failed", detail: "could not encode scroll command")
                return
            }
            view.evaluateJavaScript("window.__luchsScrollCommand && window.__luchsScrollCommand(\(json))") { _, error in
                if let error {
                    emitAck(command.id, outcome: "failed", detail: error.localizedDescription)
                } else { emitAck(command.id, outcome: "executed") }
            }
        case "reload":
            guard let mainView = webView else {
                emitAck(command.id, outcome: "failed", detail: "main view unavailable")
                return
            }
            if pageURL.isFileURL {
                do {
                    let html = try String(contentsOf: pageURL, encoding: .utf8)
                    mainView.loadHTMLString(html, baseURL: pageURL)
                } catch {
                    emitAck(command.id, outcome: "failed", detail: "could not read local page")
                    return
                }
            } else {
                mainView.reloadFromOrigin()
            }
            // Executed means the reload request was applied to WKWebView. A
            // subsequent navigation failure is distinct from command failure.
            emitAck(command.id, outcome: "executed")
        case "mouse_move", "mouse_down", "mouse_up", "scroll", "key_down", "key_up", "text", "cleanup":
            emitAck(command.id, outcome: nativeInput(command))
        default:
            emitAck(command.id, outcome: "unsupported")
        }
    }

    // MARK: Native input
    private struct HeldKey {
        let code: UInt16
        var characters: String
        let base: String
        let flags: NSEvent.ModifierFlags
        let view: WKWebView
        var pending: Bool
        var isRepeat: Bool
    }
    private struct HeldButton {
        let point: NSPoint
        let view: WKWebView
        let count: Int
    }
    private var heldKeys: [UInt64: HeldKey] = [:]
    private var heldButtons: [Int: HeldButton] = [:]
    private var pendingPress: UInt64?
    private var modifierFlags: NSEvent.ModifierFlags = []
    private var lastClick: (time: TimeInterval, point: NSPoint, button: Int, count: Int) = (0, .zero, 0, 0)
    private let traceInput = ProcessInfo.processInfo.environment["LUCHS_INPUT_TRACE"] == "1"

    private func flags(_ bits: UInt32) -> NSEvent.ModifierFlags {
        var value: NSEvent.ModifierFlags = []
        if bits & 1 != 0 { value.insert(.shift) }
        if bits & 2 != 0 { value.insert(.control) }
        if bits & 4 != 0 { value.insert(.option) }
        if bits & 8 != 0 { value.insert(.command) }
        if bits & 32 != 0 { value.insert(.capsLock) }
        return value
    }
    private func layoutCharacters(_ code: UInt16, flags: NSEvent.ModifierFlags) -> String? {
        let source = TISCopyCurrentKeyboardLayoutInputSource().takeRetainedValue()
        guard let property = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData) else { return nil }
        let data = Unmanaged<CFData>.fromOpaque(property).takeUnretainedValue()
        guard let bytes = CFDataGetBytePtr(data) else { return nil }
        let layout = UnsafeRawPointer(bytes).assumingMemoryBound(to: UCKeyboardLayout.self)
        var modifiers: UInt32 = 0
        if flags.contains(.shift) { modifiers |= UInt32(shiftKey) }
        if flags.contains(.control) { modifiers |= UInt32(controlKey) }
        if flags.contains(.option) { modifiers |= UInt32(optionKey) }
        if flags.contains(.command) { modifiers |= UInt32(cmdKey) }
        if flags.contains(.capsLock) { modifiers |= UInt32(alphaLock) }
        var dead: UInt32 = 0, length = 0
        var output = [UniChar](repeating: 0, count: 255)
        let result = UCKeyTranslate(layout, code, UInt16(kUCKeyActionDown), modifiers >> 8,
            UInt32(LMGetKbdType()), OptionBits(1 << kUCKeyTranslateNoDeadKeysBit), &dead,
            output.count, &length, &output)
        guard result == noErr else { return nil }
        return String(utf16CodeUnits: output, count: length)
    }
    private func specialCharacters(_ code: UInt16) -> String? {
        let named: [UInt16: UInt32] = [36:13, 48:9, 51:0x7f, 53:0x1b, 76:13,
            117:0xf728, 115:0xf729, 119:0xf72b, 116:0xf72c, 121:0xf72d,
            123:0xf702,124:0xf703,125:0xf701,126:0xf700,114:0xf746,71:0xf739]
        if let value = named[code] { return String(UnicodeScalar(value)!) }
        let functions: [UInt16] = [122,120,99,118,96,97,98,100,101,109,103,111,105,107,113,106]
        if let index = functions.firstIndex(of: code) { return String(UnicodeScalar(0xf704 + UInt32(index))!) }
        if isModifier(code) { return "" }
        return nil
    }
    private func isModifier(_ code: UInt16) -> Bool { (54...62).contains(code) }
    private func sendKey(_ key: HeldKey, down: Bool, flags: NSEvent.ModifierFlags? = nil) -> Bool {
        guard let window else { return false }
        let kind: NSEvent.EventType = isModifier(key.code) ? .flagsChanged : down ? .keyDown : .keyUp
        guard let event = NSEvent.keyEvent(with: kind, location: .zero, modifierFlags: flags ?? key.flags,
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil,
            characters: key.characters, charactersIgnoringModifiers: key.base, isARepeat: down && key.isRepeat,
            keyCode: key.code) else { return false }
        if traceInput { debugLog("input key code=\(key.code) down=\(down) characters=\(key.characters) flags=\(event.modifierFlags.rawValue)") }
        if kind == .flagsChanged { key.view.flagsChanged(with: event) }
        else if down {
            if event.modifierFlags.contains(.command) {
                // A standalone WebKit host needs AppKit's standard Edit menu
                // for key equivalents. Target the bound view, including popups.
                let menu = NSMenu(title: "Edit")
                for (title, action, equivalent) in [("Select All", "selectAll:", "a"), ("Copy", "copy:", "c"),
                    ("Paste", "paste:", "v"), ("Cut", "cut:", "x"), ("Undo", "undo:", "z")] {
                    let item = NSMenuItem(title: title, action: NSSelectorFromString(action), keyEquivalent: equivalent)
                    item.target = key.view
                    menu.addItem(item)
                }
                if menu.performKeyEquivalent(with: event) || key.view.performKeyEquivalent(with: event) { return true }
            }
            key.view.keyDown(with: event)
        } else { key.view.keyUp(with: event) }
        return true
    }
    private func flushPending() -> Bool {
        guard let press = pendingPress, var key = heldKeys[press] else { pendingPress = nil; return true }
        key.pending = false
        guard sendKey(key, down: true) else { return false }
        heldKeys[press] = key
        pendingPress = nil
        return true
    }
    private func mouse(_ button: Int, down: Bool, point: NSPoint, count: Int, view: WKWebView) -> Bool {
        guard let window else { return false }
        let kind: NSEvent.EventType = button == 1 ? (down ? .leftMouseDown : .leftMouseUp)
            : button == 2 ? (down ? .rightMouseDown : .rightMouseUp) : (down ? .otherMouseDown : .otherMouseUp)
        guard let initial = NSEvent.mouseEvent(with: kind, location: point, modifierFlags: modifierFlags,
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil,
            eventNumber: 0, clickCount: count, pressure: down ? 1 : 0), let cg = initial.cgEvent else { return false }
        cg.setIntegerValueField(.mouseEventButtonNumber, value: Int64(button - 1))
        guard let event = NSEvent(cgEvent: cg) else { return false }
        if traceInput { debugLog("input button=\(button) down=\(down) point=\(point) native=\(event.buttonNumber)") }
        switch kind {
        case .leftMouseDown: view.mouseDown(with: event)
        case .leftMouseUp: view.mouseUp(with: event)
        case .rightMouseDown: view.rightMouseDown(with: event)
        case .rightMouseUp: view.rightMouseUp(with: event)
        case .otherMouseDown: view.otherMouseDown(with: event)
        default: view.otherMouseUp(with: event)
        }
        return true
    }
    private func cleanup(_ scope: String) -> Bool {
        // Remove only confirmed releases. A partial failure deliberately retains
        // the remaining bindings for retry; Rust quarantines controller admission.

        for button in heldButtons.keys.sorted() {
            guard let held = heldButtons[button], mouse(button, down: false, point: held.point, count: held.count, view: held.view) else { return false }
            heldButtons.removeValue(forKey: button)
        }
        if scope == "all" {
            // Deferred printable presses were never delivered; cleanup discards
            // them instead of introducing a new character while losing focus.
            pendingPress = nil
            for press in heldKeys.keys.sorted() {
                guard let key = heldKeys[press] else { continue }
                if !key.pending && !sendKey(key, down: false, flags: []) { return false }
                heldKeys.removeValue(forKey: press)
            }
            modifierFlags = []
        }
        return true
    }
    private func nativeInput(_ command: HelperCommand) -> String {
        if command.type == "cleanup" {
            guard let scope = command.scope, scope == "all" || scope == "pointer" else { return "unsupported" }
            return cleanup(scope) ? "executed" : "failed"
        }
        guard let view = activeView, let window else { return "failed" }
        if window.firstResponder !== view { window.makeFirstResponder(view) }
        let point = NSPoint(x: command.x ?? 0, y: Double(height) - (command.y ?? 0))
        switch command.type {
        case "text":
            guard let text = command.text else { return "unsupported" }
            if command.cooperative == true, let press = pendingPress, var key = heldKeys[press] {
                key.characters = text; key.pending = false
                guard sendKey(key, down: true) else { return "failed" }
                heldKeys[press] = key; pendingPress = nil
            } else {
                guard let client = view as? NSTextInputClient else { return "unsupported" }
                client.insertText(text, replacementRange: NSRange(location: NSNotFound, length: 0))
            }
        case "key_down", "key_up":
            guard let press = command.press else { return "unsupported" }
            modifierFlags = flags(command.modifiers ?? 0)
            if command.type == "key_up" {
                guard var key = heldKeys[press] else { return "unsupported" }
                if key.pending {
                    key.pending = false
                    guard sendKey(key, down: true) else { return "failed" }
                }
                guard sendKey(key, down: false, flags: modifierFlags) else { return "failed" }
                heldKeys.removeValue(forKey: press)
                if pendingPress == press { pendingPress = nil }
            } else if command.isRepeat == true {
                guard var key = heldKeys[press], flushPending() else { return "unsupported" }
                key.isRepeat = true
                key.pending = command.cooperative == true && !key.characters.isEmpty && key.characters.unicodeScalars.allSatisfy { $0.value >= 32 && $0.value < 0xf700 } && modifierFlags.isDisjoint(with: [.control,.option,.command])
                if key.pending { pendingPress = press } else if !sendKey(key, down: true, flags: modifierFlags) { return "failed" }
                heldKeys[press] = key
            } else {
                guard heldKeys[press] == nil, let code = command.keyCode, flushPending() else { return "unsupported" }
                let chars = command.logical ?? specialCharacters(code) ?? layoutCharacters(code, flags: modifierFlags)
                let base = command.logical ?? specialCharacters(code) ?? layoutCharacters(code, flags: [])
                guard let chars, let base else { return "unsupported" }
                let pending = command.cooperative == true && !chars.isEmpty && chars.unicodeScalars.allSatisfy { $0.value >= 32 && $0.value < 0xf700 } && modifierFlags.isDisjoint(with: [.control,.option,.command])
                let key = HeldKey(code: code, characters: chars, base: base, flags: modifierFlags, view: view, pending: pending, isRepeat: false)
                if pending { pendingPress = press } else if !sendKey(key, down: true) { return "failed" }
                heldKeys[press] = key
            }
        case "mouse_down", "mouse_up":
            guard let button = command.button, (1...5).contains(button) else { return "unsupported" }
            if command.type == "mouse_up" {
                guard let held = heldButtons[button], mouse(button, down: false, point: point, count: held.count, view: held.view) else { return "failed" }
                heldButtons.removeValue(forKey: button)
            } else {
                let now = ProcessInfo.processInfo.systemUptime
                let count = lastClick.button == button && now - lastClick.time < NSEvent.doubleClickInterval && hypot(point.x - lastClick.point.x, point.y - lastClick.point.y) < 4 ? lastClick.count + 1 : 1
                guard mouse(button, down: true, point: point, count: count, view: view) else { return "failed" }
                lastClick = (now, point, button, count)
                heldButtons[button] = HeldButton(point: point, view: view, count: count)
            }
        case "mouse_move":
            let button = heldButtons.keys.sorted().first
            let target = button.flatMap { heldButtons[$0]?.view } ?? view
            let kind: NSEvent.EventType = button == 1 ? .leftMouseDragged : button == 2 ? .rightMouseDragged : button == nil ? .mouseMoved : .otherMouseDragged
            guard let initial = NSEvent.mouseEvent(with: kind, location: point, modifierFlags: modifierFlags,
                timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil,
                eventNumber: 0, clickCount: 0, pressure: button == nil ? 0 : 1), let cg = initial.cgEvent else { return "failed" }
            if let button { cg.setIntegerValueField(.mouseEventButtonNumber, value: Int64(button - 1)) }
            guard let event = NSEvent(cgEvent: cg) else { return "failed" }
            switch kind {
            case .leftMouseDragged: target.mouseDragged(with: event)
            case .rightMouseDragged: target.rightMouseDragged(with: event)
            case .otherMouseDragged: target.otherMouseDragged(with: event)
            default:
                // WKWebView inherits NSResponder's mouseMoved no-op. WebKit
                // receives hover through its own AppKit tracking-area owner.
                // WebKit currently exposes one primary mouse-moved area here.
                // Select one owner to avoid duplicate DOM events; the live
                // regression must be rerun if WebKit changes these areas.
                let selector = #selector(NSResponder.mouseMoved(with:))
                guard let owner = target.trackingAreas.first(where: { $0.options.contains(.mouseMoved) })?.owner as? NSObject,
                    owner.responds(to: selector) else { return "unsupported" }
                owner.perform(selector, with: event)
            }
            for button in heldButtons.keys { if let held = heldButtons[button] { heldButtons[button] = HeldButton(point: point, view: held.view, count: held.count) } }
        case "scroll":
            guard let dx = command.dx, let dy = command.dy, dx.isFinite, dy.isFinite,
                abs(dx) <= 32767, abs(dy) <= 32767, let px = command.pointDx, let py = command.pointDy,
                let cg = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 2, wheel1: 0, wheel2: 0, wheel3: 0) else { return "unsupported" }
            cg.setIntegerValueField(.scrollWheelEventIsContinuous, value: 1)
            // Jackstay positive means content toward its end; Quartz positive is up/left.
            cg.setIntegerValueField(.scrollWheelEventDeltaAxis1, value: -py)
            cg.setIntegerValueField(.scrollWheelEventDeltaAxis2, value: -px)
            cg.setDoubleValueField(.scrollWheelEventFixedPtDeltaAxis1, value: -dy)
            cg.setDoubleValueField(.scrollWheelEventFixedPtDeltaAxis2, value: -dx)
            cg.setIntegerValueField(.scrollWheelEventPointDeltaAxis1, value: -py)
            cg.setIntegerValueField(.scrollWheelEventPointDeltaAxis2, value: -px)
            let primaryHeight = NSScreen.screens.first?.frame.height ?? CGFloat(height)
            cg.location = CGPoint(x: window.frame.minX + point.x, y: primaryHeight - (window.frame.minY + point.y))
            guard let event = NSEvent(cgEvent: cg) else { return "failed" }
            if traceInput { debugLog("input scroll dx=\(dx) dy=\(dy) fixed=\(cg.getDoubleValueField(.scrollWheelEventFixedPtDeltaAxis1)) native=\(event.scrollingDeltaX),\(event.scrollingDeltaY) precise=\(event.hasPreciseScrollingDeltas)") }
            view.scrollWheel(with: event)
        default: return "unsupported"
        }
        return "executed"
    }

    private func capture(_ command: HelperCommand) {
        guard visible, let webView = activeView else {
            emitDrawAck(command, outcome: "executed")
            return
        }
        let requestedWidth = Int((Double(width) * scale).rounded())
        let requestedHeight = Int((Double(height) * scale).rounded())
        let snapshot = snapshotConfiguration
        let backing = window?.backingScaleFactor ?? 1
        if configuredScale != scale || configuredBacking != backing {
            snapshot.rect = CGRect(x: 0, y: 0, width: width, height: height)
            // snapshotWidth is in points. AppKit's NSImage backing is at the
            // window's backing scale, so divide to request device pixels.
            snapshot.snapshotWidth = NSNumber(value: Double(width) * scale / backing)
            configuredScale = scale
            configuredBacking = backing
        }
        let start = ProcessInfo.processInfo.systemUptime
        webView.takeSnapshot(with: snapshot) { [weak self] image, error in
            guard let self else { return }
            // A display-scale or navigation change can invalidate an in-flight
            // snapshot. Retry through the scheduler rather than terminating.
            if (self.window?.backingScaleFactor ?? 1) != backing || !self.loaded {
                self.retrySnapshot(command, "backing scale or navigation changed during snapshot", expectedTransition: true)
                return
            }
            if let error {
                self.retrySnapshot(command, "WebKit snapshot error: \(error.localizedDescription)")
                return
            }
            guard let image else {
                self.retrySnapshot(command, "snapshot returned no image")
                return
            }
            let elapsed = ProcessInfo.processInfo.systemUptime - start
            self.emit(image, width: requestedWidth, height: requestedHeight, command: command, snapshotSeconds: elapsed)
        }
    }

    private func retrySnapshot(_ command: HelperCommand, _ detail: String, expectedTransition: Bool = false) {
        configuredBacking = 0
        if let failure = snapshotRecovery.discard(detail, expectedTransition: expectedTransition) {
            debugLog(failure)
            emitDrawAck(command, outcome: "failed", detail: failure)
        } else {
            debugLog("discarding snapshot: \(detail); retrying")
            emitDrawAck(command, outcome: "executed")
            reportActivity()
        }
    }

    private func emit(_ image: NSImage, width: Int, height: Int, command: HelperCommand, snapshotSeconds: Double) {
        let publishStart = ProcessInfo.processInfo.systemUptime
        guard let representation = image.representations.first,
              representation.pixelsWide == width, representation.pixelsHigh == height else {
            retrySnapshot(command, "unexpected pixel size; requested \(width)x\(height)")
            return
        }
        guard let bitmap = arena?.slot(command), let graphicsContext = arena?.graphicsContext(command, image: image)
        else {
            emitDrawAck(command, outcome: "failed", detail: "failed to create arena BGRA context")
            return
        }
        let context = graphicsContext.cgContext
        // Draw the snapshot's native representation 1:1. Asking NSImage for a
        // screen-scaled CGImage can round 26x17 pixels down to 24x16 on Retina.
        context.setBlendMode(.copy)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = graphicsContext
        graphicsContext.compositingOperation = .copy
        let drawn = representation.draw(in: NSRect(x: 0, y: 0, width: width, height: height))
        NSGraphicsContext.restoreGraphicsState()
        if !drawn {
            retrySnapshot(command, "snapshot draw failed")
            return
        }
        snapshotRecovery.succeeded()
        let count = width * height * 4
        // FNV-1a over native words: one read pass, no previous-frame allocation.
        let words = bitmap.bindMemory(to: UInt32.self, capacity: count / 4)
        var hash: UInt64 = (14695981039346656037 ^ UInt64(width)) &* 1099511628211
        hash = (hash ^ UInt64(height)) &* 1099511628211
        for i in 0..<(count / 4) { hash = (hash ^ UInt64(words[i])) &* 1099511628211 }
        if fingerprint == hash {
            emitDrawAck(command, outcome: "executed", capture: ["published": false,
                "snapshot_ns": UInt64(snapshotSeconds * 1e9), "publish_ns": 0])
            return
        }
        fingerprint = hash
        emittedFrames += 1
        if emittedFrames == 1 { publishPageState() }
        let elapsed = ProcessInfo.processInfo.systemUptime - publishStart
        emitDrawAck(command, outcome: "executed", capture: ["published": true,
            "snapshot_ns": UInt64(snapshotSeconds * 1e9), "publish_ns": UInt64(elapsed * 1e9)],
            frame: ["format":"bgra8", "width":width, "height":height, "stride":width * 4, "len":count])
        // Rust owns --frames and shuts down only after commit. An ack followed
        // by helper exit must never turn an outstanding reservation into a frame.

    }

}

@main
private enum LuchsWebviewCapture {
    static func main() {
        let args = CommandLine.arguments
        guard args.count >= 2 else {
            fail("usage: luchs-webview-capture path/to/fragment.html [width height [frame_count fps]]")
        }
        let width = args.count >= 3 ? (Int(args[2]) ?? defaultWidth) : defaultWidth
        let height = args.count >= 4 ? (Int(args[3]) ?? defaultHeight) : defaultHeight
        let frameCount = args.count >= 5 ? (Int(args[4]) ?? defaultFrameCount) : defaultFrameCount
        // Retain the positional fps argument for older launchers. Rust now
        // schedules capture commands; the helper only validates this argument.
        let fps = args.count >= 6 ? (Int(args[5]) ?? defaultFps) : defaultFps
        guard width > 0 && height > 0 && width <= maxFrameBytes / 4 && height <= maxFrameBytes / 4 / width && frameCount >= 0 && fps > 0 else {
            fail("width, height, and fps must be positive; BGRA pixels must fit 64 MiB; frame_count must be zero or positive")
        }

        let page = args[1]
        let url: URL
        if page.hasPrefix("http://") || page.hasPrefix("https://") {
            guard let remote = URL(string: page), remote.host != nil else {
                fail("not a URL: \(page)")
            }
            url = remote
        } else {
            url = URL(fileURLWithPath: page)
            guard FileManager.default.fileExists(atPath: url.path) else {
                fail("file not found: \(url.path)")
            }
        }

        let controller = CaptureController(pageURL: url, width: width, height: height, frameCount: frameCount)
        controller.run()
    }
}
