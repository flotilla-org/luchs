import AppKit

// WebKit gates hover delivery on key status even when native events are sent
// directly. Give the embedded page a local input focus without asking AppKit
// or WindowServer to make this invisible window key or activate the helper.
// WebKit's native mouse-event tests use the same local-key distinction.
final class CaptureInputWindow: NSWindow {
    override var isKeyWindow: Bool { true }
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

