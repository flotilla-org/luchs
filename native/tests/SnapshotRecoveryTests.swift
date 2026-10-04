@main
struct SnapshotRecoveryTests {
    static func main() {
        var recovery = SnapshotRecovery()
        precondition(recovery.discard("navigation in flight", expectedTransition: true) == nil)
        precondition(recovery.failures == 0)
        precondition(recovery.discard("transient WebKit error") == nil)
        precondition(recovery.discard("unexpected pixel size") == nil)
        precondition(recovery.discard("display scale changed", expectedTransition: true) == nil)
        precondition(recovery.failures == 2)
        precondition(recovery.discard("snapshot returned no image") ==
            "snapshot failed after 3 consecutive attempts: snapshot returned no image")

        recovery.succeeded()
        precondition(recovery.failures == 0)
        precondition(recovery.discard("new transient error") == nil)
        recovery.succeeded()
        precondition(recovery.discard("unexpected pixel size") == nil)
        precondition(recovery.discard("unexpected pixel size") == nil)
        precondition(recovery.discard("unexpected pixel size") ==
            "snapshot failed after 3 consecutive attempts: unexpected pixel size")
        print("Snapshot recovery: transient retries, success reset and persistent failure passed")
    }
}
