// packaging/app-icon.swift
//
// Renders the Textify Voice app icon as a 1024x1024 PNG.
//
// The app shipped with no CFBundleIconFile at all, which means Finder drew the
// generic blank-page-on-a-white-sheet icon for it -- in the disk image, in
// Applications, and in the "Textify Voice was blocked" row in System Settings.
// For an unsigned alpha that already asks the user for an unusual amount of
// trust, looking like an unfinished binary is the wrong first impression.
//
// The mark is the site's own: a rounded-square frame with the two-tone "tx"
// (see apps/web/public/favicon.svg -- `t` in ink, `x` in grey). Voice is
// signalled by a mic badge rather than by redrawing the letterforms, so the
// icon still reads as Textify at a glance and as *which* Textify at size. It
// is deliberately not a new mark.
//
// Usage: swift packaging/app-icon.swift <out.png> [<pixel-size>]

import AppKit
import Foundation

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write("usage: app-icon.swift <out.png> [size]\n".data(using: .utf8)!)
    exit(2)
}
let outPath = args[1]
let S: CGFloat = args.count >= 3 ? CGFloat(Int(args[2]) ?? 1024) : 1024

// Brand colours, matching apps/web/public/favicon.svg and the site tokens.
let ink = NSColor(calibratedRed: 0.067, green: 0.067, blue: 0.067, alpha: 1) // #111111
let grey = NSColor(calibratedRed: 0.60, green: 0.60, blue: 0.60, alpha: 1)   // #999999
let paper = NSColor.white

let image = NSImage(size: NSSize(width: S, height: S))
image.lockFocusFlipped(true) // top-left origin

let u = S / 1024 // everything below is authored against a 1024 grid

// ---- The rounded square -------------------------------------------------
// Apple's icon grid: the shape occupies ~824pt of a 1024pt canvas, leaving the
// margin the system expects for shadow and optical alignment with other icons.
let shapeInset: CGFloat = 100 * u
let shapeRect = NSRect(x: shapeInset, y: shapeInset, width: S - 2 * shapeInset, height: S - 2 * shapeInset)
let radius = shapeRect.width * 0.2237
// White with an ink outline, matching favicon.svg's treatment rather than a
// filled dark tile: the mark IS the outlined frame, and inverting it to a solid
// block reads as a different brand at a glance.
let stroke: CGFloat = 58 * u
let squircle = NSBezierPath(
    roundedRect: shapeRect.insetBy(dx: stroke / 2, dy: stroke / 2),
    xRadius: radius - stroke / 2, yRadius: radius - stroke / 2
)
paper.setFill()
squircle.fill()
ink.setStroke()
squircle.lineWidth = stroke
squircle.stroke()

// ---- "tx" ---------------------------------------------------------------
// Two-tone exactly as the favicon: `t` in ink, `x` in grey.
let txSize: CGFloat = 430 * u
let para = NSMutableParagraphStyle()
para.alignment = .center
let font = NSFont.systemFont(ofSize: txSize, weight: .bold)
let tx = NSMutableAttributedString()
tx.append(NSAttributedString(string: "t", attributes: [
    .font: font, .foregroundColor: ink, .kern: -18 * u,
]))
tx.append(NSAttributedString(string: "x", attributes: [
    .font: font, .foregroundColor: grey,
]))
tx.addAttribute(.paragraphStyle, value: para, range: NSRange(location: 0, length: tx.length))

// Optically centred: cap-height text sits low inside its line box, and the mic
// badge adds visual weight to the lower right, so it is nudged up as well.
let txBounds = tx.boundingRect(with: NSSize(width: S, height: S), options: [.usesLineFragmentOrigin])
let txY = shapeRect.midY - txBounds.height / 2 - 92 * u
tx.draw(with: NSRect(x: 0, y: txY, width: S, height: txBounds.height + 40 * u),
        options: [.usesLineFragmentOrigin])

// ---- Mic badge ----------------------------------------------------------
// Wholly inside the frame, not straddling it. Overlapping the border means
// punching a hole in it to keep the badge legible, and a frame with a bite out
// of it reads as a rendering fault rather than as a badge.
let bc = NSPoint(x: shapeRect.maxX - 236 * u, y: shapeRect.maxY - 226 * u)
let br: CGFloat = 118 * u

ink.setFill()
NSBezierPath(ovalIn: NSRect(x: bc.x - br, y: bc.y - br, width: br * 2, height: br * 2)).fill()

// Mic glyph, drawn rather than set in a font so it does not depend on which SF
// Symbols exist on the build machine. White on the dark disc.
paper.setStroke()
paper.setFill()

// Capsule body.
let bodyW: CGFloat = 68 * u
let bodyH: CGFloat = 106 * u
let bodyRect = NSRect(x: bc.x - bodyW / 2, y: bc.y - 72 * u, width: bodyW, height: bodyH)
NSBezierPath(roundedRect: bodyRect, xRadius: bodyW / 2, yRadius: bodyW / 2).fill()

// Cradle: an arc under the body, drawn in AppKit's unflipped angle space.
let cradle = NSBezierPath()
cradle.lineWidth = 20 * u
cradle.lineCapStyle = .round
// Angles are mirrored by lockFocusFlipped, so these are the reflection of the
// unflipped 200..340 that would put the cradle under the capsule.
cradle.appendArc(withCenter: bc, radius: 66 * u, startAngle: 20, endAngle: 160, clockwise: false)
cradle.stroke()

// Stem + base.
let stem = NSBezierPath()
stem.lineWidth = 20 * u
stem.lineCapStyle = .round
stem.move(to: NSPoint(x: bc.x, y: bc.y + 66 * u))
stem.line(to: NSPoint(x: bc.x, y: bc.y + 88 * u))
stem.stroke()

let base = NSBezierPath()
base.lineWidth = 20 * u
base.lineCapStyle = .round
base.move(to: NSPoint(x: bc.x - 38 * u, y: bc.y + 88 * u))
base.line(to: NSPoint(x: bc.x + 38 * u, y: bc.y + 88 * u))
base.stroke()

image.unlockFocus()

guard let tiff = image.tiffRepresentation,
      let rep = NSBitmapImageRep(data: tiff),
      let png = rep.representation(using: .png, properties: [:]) else {
    FileHandle.standardError.write("error: PNG encoding failed\n".data(using: .utf8)!)
    exit(1)
}
try png.write(to: URL(fileURLWithPath: outPath))
print("wrote \(outPath) (\(Int(S))x\(Int(S)))")
