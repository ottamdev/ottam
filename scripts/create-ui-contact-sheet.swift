import AppKit
import Foundation

guard CommandLine.arguments.count == 3 else {
    FileHandle.standardError.write(
        Data("usage: swift scripts/create-ui-contact-sheet.swift <screenshots-dir> <output.png>\n".utf8)
    )
    exit(64)
}

let inputDirectory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let outputURL = URL(fileURLWithPath: CommandLine.arguments[2])
let fileManager = FileManager.default
let imageURLs = try fileManager.contentsOfDirectory(
    at: inputDirectory,
    includingPropertiesForKeys: nil
).filter { $0.pathExtension.lowercased() == "png" }
 .sorted { $0.lastPathComponent < $1.lastPathComponent }

guard !imageURLs.isEmpty else {
    FileHandle.standardError.write(Data("no PNG screenshots found\n".utf8))
    exit(66)
}

let images = try imageURLs.map { url -> (URL, NSImage) in
    guard let image = NSImage(contentsOf: url) else {
        throw CocoaError(.fileReadCorruptFile)
    }
    return (url, image)
}

let columns = 2
let cellWidth: CGFloat = 720
let imageHeight: CGFloat = 440
let labelHeight: CGFloat = 36
let padding: CGFloat = 20
let rows = Int(ceil(Double(images.count) / Double(columns)))
let canvasSize = NSSize(
    width: padding + CGFloat(columns) * (cellWidth + padding),
    height: padding + CGFloat(rows) * (imageHeight + labelHeight + padding)
)
let canvas = NSImage(size: canvasSize)

canvas.lockFocus()
NSColor.windowBackgroundColor.setFill()
NSBezierPath(rect: NSRect(origin: .zero, size: canvasSize)).fill()

let labelAttributes: [NSAttributedString.Key: Any] = [
    .font: NSFont.systemFont(ofSize: 16, weight: .semibold),
    .foregroundColor: NSColor.labelColor,
]

for (index, entry) in images.enumerated() {
    let column = index % columns
    let row = index / columns
    let x = padding + CGFloat(column) * (cellWidth + padding)
    let top = canvasSize.height - padding - CGFloat(row) * (imageHeight + labelHeight + padding)
    let imageRect = NSRect(x: x, y: top - imageHeight, width: cellWidth, height: imageHeight)
    let sourceSize = entry.1.size
    let scale = min(imageRect.width / sourceSize.width, imageRect.height / sourceSize.height)
    let drawSize = NSSize(width: sourceSize.width * scale, height: sourceSize.height * scale)
    let drawRect = NSRect(
        x: imageRect.midX - drawSize.width / 2,
        y: imageRect.midY - drawSize.height / 2,
        width: drawSize.width,
        height: drawSize.height
    )
    entry.1.draw(in: drawRect)
    entry.0.deletingPathExtension().lastPathComponent.draw(
        at: NSPoint(x: x, y: top - imageHeight - labelHeight + 8),
        withAttributes: labelAttributes
    )
}

canvas.unlockFocus()

guard let tiff = canvas.tiffRepresentation,
      let bitmap = NSBitmapImageRep(data: tiff),
      let png = bitmap.representation(using: .png, properties: [:]) else {
    FileHandle.standardError.write(Data("could not encode contact sheet\n".utf8))
    exit(74)
}

try fileManager.createDirectory(
    at: outputURL.deletingLastPathComponent(),
    withIntermediateDirectories: true
)
try png.write(to: outputURL, options: .atomic)
