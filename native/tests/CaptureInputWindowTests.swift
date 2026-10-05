import AppKit

// Live desktop check: scripts/test-hover-window.sh (not the headless policy suite).
@main
struct CaptureInputWindowTests {
    static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.prohibited)
        let frontmost = NSWorkspace.shared.frontmostApplication?.processIdentifier
        precondition(frontmost != nil, "requires a logged-in macOS desktop")
        let window = CaptureInputWindow(contentRect: NSRect(x: 0, y: 0, width: 640, height: 480),
            styleMask: [.borderless], backing: .buffered, defer: false)
        window.alphaValue = 0
        window.ignoresMouseEvents = true
        window.acceptsMouseMovedEvents = true
        window.orderFront(nil)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        precondition(window.isKeyWindow, "WebKit must see local input focus")
        precondition(!window.canBecomeKey && !window.canBecomeMain)
        precondition(app.keyWindow == nil && app.mainWindow == nil && !app.isActive,
            "the helper must not acquire desktop focus")
        precondition(NSWorkspace.shared.frontmostApplication?.processIdentifier == frontmost,
            "ordering the capture window must preserve the frontmost application")
        window.orderOut(nil)
        print("Capture input window: local key status, no AppKit key/main window or activation, frontmost application preserved")
    }
}
