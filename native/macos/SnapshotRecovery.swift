// Shared budget for transient WebKit errors and invalid snapshot dimensions.
// Two failed attempts may retry; the third must produce a useful diagnostic.
struct SnapshotRecovery {
    private(set) var failures = 0

    mutating func failure(_ detail: String) -> String? {
        failures += 1
        return failures >= 3
            ? "snapshot failed after \(failures) consecutive attempts: \(detail)"
            : nil
    }

    mutating func succeeded() {
        failures = 0
    }
}
