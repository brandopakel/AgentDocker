import AppKit
import Foundation

let output = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
for size in [16, 32, 64, 128, 256, 512, 1024] {
    let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    let context = NSGraphicsContext(bitmapImageRep: bitmap)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    context.cgContext.scaleBy(x: CGFloat(size) / 512, y: CGFloat(size) / 512)
    NSColor(red: 20/255, green: 121/255, blue: 209/255, alpha: 1).setFill()
    NSBezierPath(roundedRect: NSRect(x: 16, y: 16, width: 480, height: 480), xRadius: 112, yRadius: 112).fill()
    NSColor.white.setStroke()
    let edges = NSBezierPath()
    edges.move(to: NSPoint(x: 136, y: 160))
    edges.line(to: NSPoint(x: 256, y: 376))
    edges.line(to: NSPoint(x: 376, y: 160))
    edges.close()
    edges.lineWidth = 28
    edges.lineJoinStyle = .round
    edges.stroke()
    NSColor.white.setFill()
    for point in [NSPoint(x: 136, y: 160), NSPoint(x: 256, y: 376), NSPoint(x: 376, y: 160)] {
        NSBezierPath(ovalIn: NSRect(x: point.x - 42, y: point.y - 42, width: 84, height: 84)).fill()
    }
    NSColor(red: 80/255, green: 237/255, blue: 186/255, alpha: 1).setFill()
    NSBezierPath(ovalIn: NSRect(x: 226, y: 210, width: 60, height: 60)).fill()
    NSGraphicsContext.restoreGraphicsState()
    try bitmap.representation(using: .png, properties: [:])!.write(to: output.appendingPathComponent("\(size).png"))
}
