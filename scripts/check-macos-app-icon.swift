#!/usr/bin/env swift

import AppKit

enum IconVerificationStage: String {
  case swiftEntry = "swift-entry"
  case fileCheckSucceeded = "file-check-succeeded"
  case workspaceDispatched = "workspace-dispatched"
  case workspaceReturned = "workspace-returned"
  case imageSizeDispatched = "image-size-dispatched"
  case imageSizeReturned = "image-size-returned"
  case bitmapDispatched = "bitmap-dispatched"
  case bitmapReturned = "bitmap-returned"
  case contextDispatched = "context-dispatched"
  case contextReturned = "context-returned"
  case drawDispatched = "draw-dispatched"
  case drawReturned = "draw-returned"
  case pixelScanDispatched = "pixel-scan-dispatched"
  case pixelScanReturned = "pixel-scan-returned"
  case verdictRejected = "verdict-rejected"
  case verdictAccepted = "verdict-accepted"
}

// Closed CI observations exclude application paths, arguments, and native errors.
func observeIconStage(
  _ stage: IconVerificationStage,
  counts: (opaque: Int, dark: Int, pale: Int)? = nil,
  dimensions: (width: Int, height: Int)? = nil
) {
  var fields = "\"stage\":\"\(stage.rawValue)\""
  if let counts = counts {
    let cap = 1_048_576
    fields += ",\"opaque\":\(min(counts.opaque, cap)),\"dark\":\(min(counts.dark, cap)),\"pale\":\(min(counts.pale, cap))"
    fields += ",\"countsCapped\":\(counts.opaque > cap || counts.dark > cap || counts.pale > cap)"
  }
  if let dimensions = dimensions {
    let cap = 16_384
    fields += ",\"width\":\(min(dimensions.width, cap)),\"height\":\(min(dimensions.height, cap))"
    fields += ",\"dimensionsCapped\":\(dimensions.width > cap || dimensions.height > cap)"
  }
  // Logging remains optional; write or flush refusal cannot replace the verdict.
  fputs("mac-icon-observation {\(fields)}\n", stderr)
  fflush(stderr)
}

// Rasterize the selected Finder representation, rather than serializing every
// representation through TIFF. Keep the pixel grid at 1024 square and the icon
// at 256 points, independent of the runner display scale.
func renderIcon(_ image: NSImage, observing: Bool = true) -> NSBitmapImageRep? {
  if observing { observeIconStage(.bitmapDispatched) }
  let bitmap = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: 1024, pixelsHigh: 1024,
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
  )
  if observing { observeIconStage(.bitmapReturned) }
  guard let bitmap = bitmap else { return nil }
  if observing { observeIconStage(.contextDispatched) }
  let context = NSGraphicsContext(bitmapImageRep: bitmap)
  if observing { observeIconStage(.contextReturned) }
  guard let context = context else { return nil }
  NSGraphicsContext.saveGraphicsState()
  defer { NSGraphicsContext.restoreGraphicsState() }
  NSGraphicsContext.current = context
  context.cgContext.clear(CGRect(x: 0, y: 0, width: 1024, height: 1024))
  context.cgContext.scaleBy(x: 4, y: 4)
  if observing { observeIconStage(.drawDispatched) }
  image.draw(
    in: NSRect(x: 0, y: 0, width: 256, height: 256),
    from: .zero, operation: .copy, fraction: 1
  )
  if observing { observeIconStage(.drawReturned) }
  return bitmap
}

struct IconPixelCounts {
  let opaque: Int
  let dark: Int
  let pale: Int

  var exitStatus: Int32 {
    if opaque == 0 { return 2 }
    let darkRatio = Double(dark) / Double(opaque)
    let paleRatio = Double(pale) / Double(opaque)
    return darkRatio < 0.70 || paleRatio > 0.25 ? 1 : 0
  }
}

func scanIcon(_ bitmap: NSBitmapImageRep) -> IconPixelCounts {
  var opaque = 0
  var dark = 0
  var pale = 0
  for y in 0..<bitmap.pixelsHigh {
    for x in 0..<bitmap.pixelsWide {
      guard
        let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB),
        color.alphaComponent > 0.5
      else { continue }
      opaque += 1
      let luminance =
        0.2126 * color.redComponent +
        0.7152 * color.greenComponent +
        0.0722 * color.blueComponent
      if luminance < 0.15 { dark += 1 }
      if luminance > 0.70 { pale += 1 }
    }
  }
  return IconPixelCounts(opaque: opaque, dark: dark, pale: pale)
}

// This CI-only entry checks the real renderer and verdict together. It cannot
// replace the subsequent check of the exact application mounted from the DMG.
func runRasterFixtures() -> Bool {
  let fixtures: [(String, CGFloat?, CGFloat?, Int32)] = [
    ("black", 256, nil, 0),
    ("white", nil, 256, 1),
    ("transparent", nil, nil, 2),
    ("dark-below-limit", 176, nil, 1),
    ("pale-above-limit", 184, 256, 1),
    ("accepted-border", 192, 256, 0),
  ]
  for (name, blackWidth, whiteWidth, expected) in fixtures {
    let image = NSImage(size: NSSize(width: 256, height: 256), flipped: false) { rect in
      if let whiteWidth = whiteWidth {
        NSColor.white.setFill()
        NSRect(x: 0, y: 0, width: whiteWidth, height: rect.height).fill()
      } else if blackWidth != nil {
        NSColor.gray.setFill()
        rect.fill()
      }
      if let blackWidth = blackWidth {
        NSColor.black.setFill()
        NSRect(x: 0, y: 0, width: blackWidth, height: rect.height).fill()
      }
      return true
    }
    guard let bitmap = renderIcon(image, observing: false),
      bitmap.pixelsWide == 1024, bitmap.pixelsHigh == 1024
    else { return false }
    let counts = scanIcon(bitmap)
    guard counts.exitStatus == expected else { return false }
    if name == "black" && (counts.opaque != 1_048_576 || counts.dark != counts.opaque || counts.pale != 0) {
      return false
    }
    if name == "white" && (counts.opaque != 1_048_576 || counts.pale != counts.opaque || counts.dark != 0) {
      return false
    }
    if name == "transparent" && counts.opaque != 0 { return false }
  }
  // Independent edge expectations retain both ratio limits and the empty rule.
  return IconPixelCounts(opaque: 100, dark: 70, pale: 25).exitStatus == 0
    && IconPixelCounts(opaque: 100, dark: 69, pale: 0).exitStatus == 1
    && IconPixelCounts(opaque: 100, dark: 74, pale: 26).exitStatus == 1
    && IconPixelCounts(opaque: 0, dark: 0, pale: 0).exitStatus == 2
}

observeIconStage(.swiftEntry)

if CommandLine.arguments.count == 2 && CommandLine.arguments[1] == "--self-test" {
  guard runRasterFixtures() else {
    fputs("FAIL: Finder icon raster fixtures\n", stderr)
    exit(1)
  }
  print("PASS: Finder icon raster fixtures")
  exit(0)
}

guard CommandLine.arguments.count == 2 else {
  fputs("Usage: check-macos-app-icon.swift /path/to/Coddy Studio.app\n", stderr)
  exit(2)
}

let appPath = CommandLine.arguments[1]
guard FileManager.default.fileExists(atPath: appPath) else {
  fputs("Application bundle does not exist: \(appPath)\n", stderr)
  exit(2)
}

observeIconStage(.fileCheckSucceeded)
observeIconStage(.workspaceDispatched)
let image = NSWorkspace.shared.icon(forFile: appPath)
observeIconStage(.workspaceReturned)
observeIconStage(.imageSizeDispatched)
image.size = NSSize(width: 256, height: 256)
observeIconStage(.imageSizeReturned)
guard let bitmap = renderIcon(image) else {
  fputs("Could not render application icon: \(appPath)\n", stderr)
  exit(2)
}

observeIconStage(.pixelScanDispatched, dimensions: (width: bitmap.pixelsWide, height: bitmap.pixelsHigh))
let counts = scanIcon(bitmap)
let opaque = counts.opaque
let dark = counts.dark
let pale = counts.pale
observeIconStage(.pixelScanReturned, counts: (opaque: opaque, dark: dark, pale: pale))
guard opaque > 0 else {
  observeIconStage(.verdictRejected)
  fputs("Rendered application icon has no opaque pixels: \(appPath)\n", stderr)
  exit(2)
}

let darkRatio = Double(dark) / Double(opaque)
let paleRatio = Double(pale) / Double(opaque)
print(
  String(
    format: "Finder-rendered icon: dark %.1f%%, pale %.1f%%",
    darkRatio * 100,
    paleRatio * 100
  )
)
if counts.exitStatus == 1 {
  observeIconStage(.verdictRejected)
  fputs("FAIL: macOS adds a large pale surround to the Coddy Studio icon\n", stderr)
  exit(1)
}
observeIconStage(.verdictAccepted)
print("PASS: Coddy Studio renders as a predominantly black macOS icon")
