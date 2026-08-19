// packaging/dmg-background.swift
//
// Renders the background image for the .dmg window: the drag-to-Applications
// layout plus the three steps a user needs when macOS blocks the app.
//
// Why this exists at all. This alpha is not notarized, so the first launch is
// refused. On macOS 15 (Sequoia) and later, Apple REMOVED the old
// Control-click -> Open escape hatch, and the dialog the user gets has exactly
// one button on it: "Done". There is no affordance in it, anywhere, that hints
// at what to do next -- so a user who does not already know about
// System Settings > Privacy & Security concludes the app is broken and throws
// it away. The app itself cannot tell them, because the whole point is that it
// was never allowed to run.
//
// The disk image window is therefore the LAST surface we control before that
// dead end, which makes it the right place to put the instructions.
//
// Usage: swift packaging/dmg-background.swift <out.png> [<version>]
//
// Rendered at 2x into a 640x420-point window (see build-bundle.sh's AppleScript
// bounds) so it stays sharp on Retina displays.

import AppKit
import Foundation

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write("usage: dmg-background.swift <out.png> [version]\n".data(using: .utf8)!)
    exit(2)
}
let outPath = args[1]
let version = args.count >= 3 ? args[2] : ""

// Window geometry in points. Must match the AppleScript window bounds and icon
// positions in build-bundle.sh -- the icons are placed ON this artwork, so the
// two drift apart silently if only one is edited.
let W: CGFloat = 640
let H: CGFloat = 420
let scale: CGFloat = 2

let ink = NSColor(calibratedRed: 0.07, green: 0.07, blue: 0.07, alpha: 1)
let muted = NSColor(calibratedRed: 0.42, green: 0.42, blue: 0.44, alpha: 1)
let hairline = NSColor(calibratedRed: 0.85, green: 0.85, blue: 0.86, alpha: 1)
let warnBG = NSColor(calibratedRed: 0.99, green: 0.96, blue: 0.89, alpha: 1)
let warnInk = NSColor(calibratedRed: 0.45, green: 0.32, blue: 0.05, alpha: 1)

let image = NSImage(size: NSSize(width: W, height: H))
image.lockFocusFlipped(true) // top-left origin: matches how the layout is reasoned about

// Background.
NSColor.white.setFill()
NSRect(x: 0, y: 0, width: W, height: H).fill()

func draw(_ text: String, x: CGFloat, y: CGFloat, size: CGFloat, weight: NSFont.Weight,
          color: NSColor, width: CGFloat = W - 80, align: NSTextAlignment = .left) {
    let para = NSMutableParagraphStyle()
    para.alignment = align
    para.lineSpacing = 3
    let attrs: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: size, weight: weight),
        .foregroundColor: color,
        .paragraphStyle: para,
    ]
    NSAttributedString(string: text, attributes: attrs)
        .draw(with: NSRect(x: x, y: y, width: width, height: 200),
              options: [.usesLineFragmentOrigin])
}

// ---- Header -------------------------------------------------------------
draw("Textify Voice", x: 40, y: 26, size: 22, weight: .bold, color: ink)
draw(version.isEmpty ? "macOS alpha" : "macOS alpha · v\(version)",
     x: 40, y: 54, size: 12, weight: .regular, color: muted)

// ---- Step 1: the drag ---------------------------------------------------
// The icons themselves are positioned by Finder at y=210 (centres); this line
// sits above them and the arrow between them is drawn below.
draw("1.  Drag Textify Voice into Applications",
     x: 40, y: 96, size: 13, weight: .semibold, color: ink)

// Arrow between the two icon slots (x=160 and x=480 in Finder coordinates).
let arrowY: CGFloat = 210
let path = NSBezierPath()
path.move(to: NSPoint(x: 250, y: arrowY))
path.line(to: NSPoint(x: 386, y: arrowY))
hairline.setStroke()
path.lineWidth = 2
path.stroke()
let head = NSBezierPath()
head.move(to: NSPoint(x: 396, y: arrowY))
head.line(to: NSPoint(x: 380, y: arrowY - 7))
head.line(to: NSPoint(x: 380, y: arrowY + 7))
head.close()
hairline.setFill()
head.fill()

// ---- Step 2: the part that loses people --------------------------------
let boxY: CGFloat = 268
let box = NSBezierPath(roundedRect: NSRect(x: 40, y: boxY, width: W - 80, height: 116),
                       xRadius: 10, yRadius: 10)
warnBG.setFill()
box.fill()
hairline.setStroke()
box.lineWidth = 1
box.stroke()

draw("2.  The first time you open it, macOS will refuse",
     x: 60, y: boxY + 16, size: 13, weight: .semibold, color: warnInk)
draw("“Apple could not verify Textify Voice is free of malware.” That is expected — this alpha "
   + "isn’t notarized yet. The dialog’s only button is Done. Click it.",
     x: 60, y: boxY + 38, size: 11.5, weight: .regular, color: warnInk, width: W - 120)

draw("3.  Then allow it once:  System Settings → Privacy & Security →"
   + "  scroll to Security → “Open Anyway”",
     x: 60, y: boxY + 78, size: 12, weight: .semibold, color: warnInk, width: W - 120)

image.unlockFocus()

// ---- Write the PNG at 2x ------------------------------------------------
guard let tiff = image.tiffRepresentation,
      let src = NSBitmapImageRep(data: tiff) else {
    FileHandle.standardError.write("error: could not rasterise the background\n".data(using: .utf8)!)
    exit(1)
}
let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil,
    pixelsWide: Int(W * scale), pixelsHigh: Int(H * scale),
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .calibratedRGB, bytesPerRow: 0, bitsPerPixel: 0
)!
rep.size = NSSize(width: W, height: H) // point size -> Finder scales it to the window
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
src.draw(in: NSRect(x: 0, y: 0, width: W, height: H))
NSGraphicsContext.restoreGraphicsState()

guard let png = rep.representation(using: .png, properties: [:]) else {
    FileHandle.standardError.write("error: PNG encoding failed\n".data(using: .utf8)!)
    exit(1)
}
try png.write(to: URL(fileURLWithPath: outPath))
print("wrote \(outPath) (\(Int(W * scale))x\(Int(H * scale)))")
