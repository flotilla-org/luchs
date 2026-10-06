import Foundation
import Jackstay

@main
struct WriterImportTests {
    static func main() throws {
        var config = ft_cpu_producer_config()
        config.resource_capacity = 8
        config.retained_history = 1
        config.producer_reserve = 1
        config.max_incarnations = 1
        config.payload_capacity = 16
        config.memory_budget = 1 << 20
        config.drain_timeout_ns = 1_000_000_000
        var producer: OpaquePointer?
        precondition(ft_cpu_producer_create(&config, &producer) == FT_STATUS_OK)
        defer { precondition(ft_cpu_producer_destroy(&producer) == FT_STATUS_OK) }
        var export: OpaquePointer?
        var descriptor = ft_cpu_writer_descriptor()
        var object: ft_os_object = FT_OS_OBJECT_NONE
        precondition(ft_cpu_producer_export_writer(producer, &export, &descriptor, &object) == FT_STATUS_OK)
        defer {
            close(object)
            precondition(ft_cpu_writer_export_destroy(&export) == FT_STATUS_OK)
        }
        let scope = withUnsafeBytes(of: descriptor.arena_scope) { Array($0) }
        let valid = WriterLayout(arena_scope: scope, generation: descriptor.generation,
            map_len: descriptor.map_len, slot_capacity: descriptor.slot_capacity, slots: descriptor.slots)
        let importedFD = dup(object)
        precondition(importedFD >= 0)
        var writer: OpaquePointer? = try importWriter(valid, fd: importedFD)
        precondition(ft_cpu_writer_destroy(&writer) == FT_STATUS_OK && writer == nil)
        precondition(fcntl(importedFD, F_GETFD) == -1 && errno == EBADF)

        // A malformed JSON scope fails before C import; invalid layout fields
        // fail inside Jackstay. Both paths must relinquish their received fd.
        for count in [0, 15, 17] {
            let layout = WriterLayout(arena_scope: Array(repeating: 0, count: count),
                generation: valid.generation, map_len: valid.map_len,
                slot_capacity: valid.slot_capacity, slots: valid.slots)
            try rejected(layout, object: object, malformedScope: true)
        }
        let invalidLayouts = [
            WriterLayout(arena_scope: scope, generation: 0, map_len: valid.map_len,
                slot_capacity: valid.slot_capacity, slots: valid.slots),
            WriterLayout(arena_scope: scope, generation: valid.generation, map_len: 0,
                slot_capacity: valid.slot_capacity, slots: valid.slots),
            WriterLayout(arena_scope: scope, generation: valid.generation, map_len: valid.map_len,
                slot_capacity: 0, slots: valid.slots),
            WriterLayout(arena_scope: scope, generation: valid.generation, map_len: valid.map_len,
                slot_capacity: valid.slot_capacity, slots: 0),
            WriterLayout(arena_scope: scope, generation: valid.generation, map_len: 1,
                slot_capacity: valid.slot_capacity, slots: valid.slots),
        ]
        for layout in invalidLayouts { try rejected(layout, object: object, malformedScope: false) }
        print("Writer import: valid export, malformed scope, invalid layouts and fd ownership passed")
    }

    static func rejected(_ layout: WriterLayout, object: Int32, malformedScope: Bool) throws {
        let fd = dup(object)
        precondition(fd >= 0)
        do {
            var writer: OpaquePointer? = try importWriter(layout, fd: fd)
            ft_cpu_writer_destroy(&writer)
            preconditionFailure("invalid writer offer was accepted")
        } catch let error as WriterImportError {
            switch error {
            case .malformedScope: precondition(malformedScope)
            case .status(let status):
                precondition(!malformedScope && status == FT_STATUS_ERROR)
                precondition(error.description == "Jackstay writer import failed (FT_STATUS_ERROR \(status))")
            }
        }
        precondition(fcntl(fd, F_GETFD) == -1 && errno == EBADF, "rejected import leaked the received fd")
        precondition(fcntl(object, F_GETFD) >= 0, "import closed the export's original fd")
    }
}
