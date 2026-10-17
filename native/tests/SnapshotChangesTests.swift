import Foundation

@main
struct SnapshotChangesTests {
    static func main() {
        var changes = SnapshotChanges()
        var pixels = [UInt8](repeating: 0, count: 32 * 32 * 4)
        func changed(_ width: Int = 32, _ height: Int = 32) -> Bool {
            pixels.withUnsafeBytes { changes.changed($0.baseAddress!, width: width, height: height) }
        }
        precondition(changed())
        precondition(!changed())
        // Check both ends and alpha: a partial scan must not miss a change.
        for index in [0, pixels.count / 2, pixels.count - 1] {
            pixels[index] = 255
            precondition(changed())
            precondition(!changed())
        }
        // Same byte count with different geometry must still be published.
        precondition(changed(64, 16))
        precondition(!changed(64, 16))
        changes.invalidate()
        precondition(changed(64, 16))
        precondition(!changed(64, 16))
        pixels = [UInt8](repeating: 127, count: 16 * 16 * 4)
        precondition(changed(16, 16))
        precondition(!changed(16, 16))
        // Wide but small images are valid under the byte cap.
        pixels = [UInt8](repeating: 42, count: 32768 * 4)
        precondition(changed(32768, 1))
        precondition(!changed(32768, 1))
        print("Snapshot changes: exact pixels, geometry, invalidation and resized ownership passed")
    }
}
