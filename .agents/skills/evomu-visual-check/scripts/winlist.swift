// List on-screen application windows: "<id>\t<owner>\t<title>". Optional argument filters by substring (case-insensitive).
// Usage: swift winlist.swift [filter]
import CoreGraphics
import Foundation

let filter = CommandLine.arguments.count > 1 ? CommandLine.arguments[1].lowercased() : ""
let opts = CGWindowListOption(arrayLiteral: .optionOnScreenOnly, .excludeDesktopElements)
for w in (CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]]) ?? [] {
    let owner = w[kCGWindowOwnerName as String] as? String ?? ""
    let name = w[kCGWindowName as String] as? String ?? ""
    let id = w[kCGWindowNumber as String] as? Int ?? 0
    let layer = w[kCGWindowLayer as String] as? Int ?? 0
    guard layer == 0 else { continue }
    if filter.isEmpty || owner.lowercased().contains(filter) || name.lowercased().contains(filter) {
        print("\(id)\t\(owner)\t\(name)")
    }
}
