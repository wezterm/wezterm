import AppKit
import CoreGraphics

let pid = Int32(CommandLine.arguments[1])!
let windows = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
// Pick the largest layer-0 window for the pid that isn't a thin
// menu-bar overlay. WezTerm spawns a few invisible/overlay windows
// alongside the real one.
var bestId: UInt32 = 0
var bestArea: CGFloat = 0
for w in windows {
    guard let owner = w[kCGWindowOwnerPID as String] as? Int32, owner == pid else { continue }
    guard let layer = w[kCGWindowLayer as String] as? Int, layer == 0 else { continue }
    guard let bounds = w[kCGWindowBounds as String] as? [String: CGFloat] else { continue }
    let h = bounds["Height"] ?? 0
    let width = bounds["Width"] ?? 0
    if h < 100 || width < 100 { continue }
    let area = h * width
    if area > bestArea {
        bestArea = area
        bestId = w[kCGWindowNumber as String] as? UInt32 ?? 0
    }
}
if bestId == 0 { exit(1) }
print(bestId)
