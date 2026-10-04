import Foundation

@main
struct PageAffordancesTests {
    static func main() throws {
        let temporary = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: temporary) }
        let directory = temporary.appendingPathComponent("site")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let start = directory.appendingPathComponent("index.html")
        let sibling = directory.appendingPathComponent("space name.html")
        let outside = temporary.appendingPathComponent("outside.html")
        for file in [start, sibling, outside] { try "page".write(to: file, atomically: true, encoding: .utf8) }
        let escape = directory.appendingPathComponent("escape.html")
        try FileManager.default.createSymbolicLink(at: escape, withDestinationURL: outside)
        let escapeDirectory = directory.appendingPathComponent("escape-directory")
        try FileManager.default.createSymbolicLink(at: escapeDirectory, withDestinationURL: temporary)
        let localhost = sibling.absoluteString.replacingOccurrences(of: "file:///", with: "file://localhost/")
        let localhostEscape = escape.absoluteString.replacingOccurrences(of: "file:///", with: "file://localhost/")
        let cases: [(String, Bool)] = [
            ("http://example.com", true), ("https://example.com/path", true),
            (sibling.absoluteString, true), (sibling.absoluteString + "#section", true),
            (outside.absoluteString, false), (escape.absoluteString, false),
            (escapeDirectory.appendingPathComponent("outside.html").absoluteString, false),
            (localhost, true), (localhost + "#section", true), (localhostEscape, false),
            (directory.absoluteString + "/../outside.html", false),
            (directory.absoluteString + "/%2e%2e/outside.html", false),
            ("file://remote/tmp/page.html", false), ("https://", false),
            ("javascript:alert(1)", false), ("data:text/html,hello", false),
            ("ftp://example.com", false), ("about:blank", false), ("not a URL", false)
        ]
        for (value, allowed) in cases {
            precondition((allowedNavigationURL(value, startupPage: start) != nil) == allowed, value)
        }
        precondition(allowedNavigationURL(sibling.absoluteString, startupPage: URL(string: "https://example.com")!) == nil)
        print("Page affordances: engine URL policy accept/reject and symlink containment passed")
    }
}
