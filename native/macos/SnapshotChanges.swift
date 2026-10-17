import Foundation

/// Keep an owned image for exact change detection. libc's vectorized comparison
/// and copy avoid a serial hash dependency for every pixel. Never retain an
/// arena pointer: that slot belongs to the producer again after acknowledgement.
struct SnapshotChanges {
    private var pixels: [UInt8] = []
    private var width = 0
    private var height = 0

    mutating func invalidate() {
        width = 0
        height = 0
    }

    mutating func changed(_ bytes: UnsafeRawPointer, width: Int, height: Int) -> Bool {
        let limit = 64 * 1024 * 1024
        precondition(width > 0 && height > 0 && width <= limit / 4 && height <= limit / 4 / width)
        let count = width * height * 4
        let resized = self.width != width || self.height != height
        self.width = width
        self.height = height
        if pixels.count != count {
            pixels = Array(unsafeUninitializedCapacity: count) { buffer, initialized in
                memcpy(buffer.baseAddress!, bytes, count)
                initialized = count
            }
            return true
        }
        return pixels.withUnsafeMutableBytes { previous in
            let changed = resized || memcmp(previous.baseAddress!, bytes, count) != 0
            if changed { memcpy(previous.baseAddress!, bytes, count) }
            return changed
        }
    }
}
