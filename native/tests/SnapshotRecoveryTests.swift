@main
struct SnapshotRecoveryTests {
    static func main() {
        var recovery = SnapshotRecovery()
        precondition(recovery.failure("transient WebKit error") == nil)
        precondition(recovery.failure("unexpected pixel size") == nil)
        precondition(recovery.failure("snapshot returned no image") ==
            "snapshot failed after 3 consecutive attempts: snapshot returned no image")

        recovery.succeeded()
        precondition(recovery.failures == 0)
        precondition(recovery.failure("new transient error") == nil)
        recovery.succeeded()
        precondition(recovery.failure("unexpected pixel size") == nil)
        precondition(recovery.failure("unexpected pixel size") == nil)
        precondition(recovery.failure("unexpected pixel size") ==
            "snapshot failed after 3 consecutive attempts: unexpected pixel size")
        print("Snapshot recovery: transient retries, success reset and persistent failure passed")
    }
}
