#!/usr/bin/env bash
# Generate the transparent rounded SeeCut brand image and, optionally, a
# macOS iconset from the existing astronaut artwork.
#
# Usage:
#   scripts/generate-seecut-logo.sh [source.png] [brand-output.png] [iconset-dir]
#
# Defaults:
#   source       src/crates/concat/ui/assets/seecut-astronaut.png
#   brand-output src/crates/concat/ui/assets/seecut-logo.png
#
# The source artwork is drawn at its original square size and clipped to a
# 19% rounded rectangle. When an iconset directory is supplied, a 1024px
# transparent canvas is made with a 7% margin on every side, matching the
# measured macOS icon treatment. A restrained shadow is applied only to the
# system icon canvas; all iconset sizes are then derived from that one canvas.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="${1:-$ROOT/src/crates/concat/ui/assets/seecut-astronaut.png}"
BRAND_OUTPUT="${2:-$ROOT/src/crates/concat/ui/assets/seecut-logo.png}"
ICONSET_DIR="${3:-}"

[[ -f "$SOURCE" ]] || { echo "error: source image not found: $SOURCE" >&2; exit 1; }
mkdir -p "$(dirname "$BRAND_OUTPUT")"
if [[ -n "$ICONSET_DIR" ]]; then
  mkdir -p "$ICONSET_DIR"
fi

swift - "$SOURCE" "$BRAND_OUTPUT" "$ICONSET_DIR" <<'SWIFT'
import CoreGraphics
import Foundation
import ImageIO

let arguments = CommandLine.arguments
guard arguments.count >= 3 else {
    fputs("usage: generate-seecut-logo.sh <source.png> <brand-output.png> [iconset-dir]\n", stderr)
    exit(2)
}

let sourceURL = URL(fileURLWithPath: arguments[1])
let brandURL = URL(fileURLWithPath: arguments[2])
let iconsetPath = arguments.count >= 4 ? arguments[3] : ""

let roundedCornerRatio: CGFloat = 0.19
let iconCanvasSize = 1024
let iconMarginRatio: CGFloat = 0.07

func loadImage(_ url: URL) throws -> CGImage {
    guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
        throw NSError(domain: "SeeCutLogo", code: 1, userInfo: [NSLocalizedDescriptionKey: "unable to read image: \(url.path)"])
    }
    return image
}

func makeContext(size: Int) throws -> CGContext {
    guard let colorSpace = CGColorSpace(name: CGColorSpace.sRGB),
          let context = CGContext(
              data: nil,
              width: size,
              height: size,
              bitsPerComponent: 8,
              bytesPerRow: size * 4,
              space: colorSpace,
              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
          ) else {
        throw NSError(domain: "SeeCutLogo", code: 2, userInfo: [NSLocalizedDescriptionKey: "unable to create RGBA context"])
    }
    context.clear(CGRect(x: 0, y: 0, width: size, height: size))
    context.interpolationQuality = .high
    return context
}

func roundedBrandImage(from source: CGImage) throws -> CGImage {
    guard source.width == source.height else {
        throw NSError(domain: "SeeCutLogo", code: 3, userInfo: [NSLocalizedDescriptionKey: "source image must be square"])
    }
    let size = source.width
    let context = try makeContext(size: size)
    let bounds = CGRect(x: 0, y: 0, width: size, height: size)
    let radius = CGFloat(size) * roundedCornerRatio
    context.saveGState()
    context.addPath(CGPath(roundedRect: bounds, cornerWidth: radius, cornerHeight: radius, transform: nil))
    context.clip()
    context.draw(source, in: bounds)
    context.restoreGState()
    guard let image = context.makeImage() else {
        throw NSError(domain: "SeeCutLogo", code: 4, userInfo: [NSLocalizedDescriptionKey: "unable to render brand image"])
    }
    return image
}

func iconCanvas(from brand: CGImage) throws -> CGImage {
    let context = try makeContext(size: iconCanvasSize)
    let canvas = CGFloat(iconCanvasSize)
    let margin = canvas * iconMarginRatio
    let side = canvas - 2 * margin
    context.saveGState()
    context.setShadow(
        offset: CGSize(width: 0, height: -12),
        blur: 22,
        color: CGColor(red: 0, green: 0, blue: 0, alpha: 0.22)
    )
    context.draw(brand, in: CGRect(x: margin, y: margin, width: side, height: side))
    context.restoreGState()
    guard let image = context.makeImage() else {
        throw NSError(domain: "SeeCutLogo", code: 5, userInfo: [NSLocalizedDescriptionKey: "unable to render icon canvas"])
    }
    return image
}

func resized(_ image: CGImage, to size: Int) throws -> CGImage {
    let context = try makeContext(size: size)
    context.draw(image, in: CGRect(x: 0, y: 0, width: size, height: size))
    guard let result = context.makeImage() else {
        throw NSError(domain: "SeeCutLogo", code: 6, userInfo: [NSLocalizedDescriptionKey: "unable to resize icon"])
    }
    return result
}

func writePNG(_ image: CGImage, to url: URL) throws {
    guard let destination = CGImageDestinationCreateWithURL(url as CFURL, "public.png" as CFString, 1, nil) else {
        throw NSError(domain: "SeeCutLogo", code: 7, userInfo: [NSLocalizedDescriptionKey: "unable to create PNG destination: \(url.path)"])
    }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else {
        throw NSError(domain: "SeeCutLogo", code: 8, userInfo: [NSLocalizedDescriptionKey: "unable to write PNG: \(url.path)"])
    }
}

do {
    let source = try loadImage(sourceURL)
    let brand = try roundedBrandImage(from: source)
    try writePNG(brand, to: brandURL)

    if !iconsetPath.isEmpty {
        let iconsetURL = URL(fileURLWithPath: iconsetPath, isDirectory: true)
        try FileManager.default.createDirectory(at: iconsetURL, withIntermediateDirectories: true)
        let canvas = try iconCanvas(from: brand)
        let specs: [(Int, String)] = [
            (16, "icon_16x16.png"),
            (32, "icon_16x16@2x.png"),
            (32, "icon_32x32.png"),
            (64, "icon_32x32@2x.png"),
            (128, "icon_128x128.png"),
            (256, "icon_128x128@2x.png"),
            (256, "icon_256x256.png"),
            (512, "icon_256x256@2x.png"),
            (512, "icon_512x512.png"),
            (1024, "icon_512x512@2x.png")
        ]
        for (size, name) in specs {
            let icon = size == iconCanvasSize ? canvas : try resized(canvas, to: size)
            try writePNG(icon, to: iconsetURL.appendingPathComponent(name))
        }
    }
} catch {
    fputs("error: \(error.localizedDescription)\n", stderr)
    exit(1)
}
SWIFT

echo "wrote $BRAND_OUTPUT"
if [[ -n "$ICONSET_DIR" ]]; then
  echo "wrote iconset $ICONSET_DIR"
fi
