import Foundation
import Jackstay

struct WriterLayout: Decodable {
    let arena_scope: [UInt8]
    let generation: UInt64
    let map_len: UInt64
    let slot_capacity: UInt64
    let slots: UInt32
}

enum WriterImportError: Error, CustomStringConvertible {
    case malformedScope
    case status(ft_status)

    var description: String {
        switch self {
        case .malformedScope: return "arena scope must contain 16 bytes"
        case .status(let status):
            let name: String
            switch status {
            case FT_STATUS_INVALID_ARGUMENT: name = "FT_STATUS_INVALID_ARGUMENT"
            case FT_STATUS_ERROR: name = "FT_STATUS_ERROR"
            case FT_STATUS_CLOSED: name = "FT_STATUS_CLOSED"
            default: name = "unknown status"
            }
            return "Jackstay writer import failed (\(name) \(status))"
        }
    }
}

// Takes ownership of fd on every outcome. Jackstay validates the payload layout;
// the scope length check is only needed to copy JSON bytes into the C array.
func importWriter(_ layout: WriterLayout, fd: Int32) throws -> OpaquePointer {
    var object: ft_os_object = fd
    defer { if object != FT_OS_OBJECT_NONE { close(object) } }
    guard layout.arena_scope.count == 16 else { throw WriterImportError.malformedScope }
    var descriptor = ft_cpu_writer_descriptor()
    withUnsafeMutableBytes(of: &descriptor.arena_scope) { $0.copyBytes(from: layout.arena_scope) }
    descriptor.generation = layout.generation
    descriptor.map_len = layout.map_len
    descriptor.slot_capacity = layout.slot_capacity
    descriptor.slots = layout.slots
    var handle: OpaquePointer?
    let status = ft_cpu_writer_import(&descriptor, &object, &handle)
    guard status == FT_STATUS_OK, let handle else { throw WriterImportError.status(status) }
    return handle
}
