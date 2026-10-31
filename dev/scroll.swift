// Usage: scroll <x> <y> <lines>
// Moves the pointer to screen point (x, y) and posts <lines> one-line
// scroll-wheel events there; a negative count scrolls down. Built and run
// by dev/scroll.sh.
import CoreGraphics
import Foundation

let args = CommandLine.arguments
let point = CGPoint(x: Double(args[1])!, y: Double(args[2])!)
let lines = Int32(args[3])!

CGEvent(
    mouseEventSource: nil, mouseType: .mouseMoved,
    mouseCursorPosition: point, mouseButton: .left
)?.post(tap: .cghidEventTap)
usleep(100_000)
for _ in 0..<abs(lines) {
    CGEvent(
        scrollWheelEvent2Source: nil, units: .line, wheelCount: 1,
        wheel1: lines.signum(), wheel2: 0, wheel3: 0
    )?.post(tap: .cghidEventTap)
    usleep(30_000)
}
usleep(300_000)
