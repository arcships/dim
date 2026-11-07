// Generates the OCR fixtures beside this file: `latin.png`, drawn text an
// engine recognizes, and `mixed.pdf`, whose first page carries its own text
// layer and whose second page is an image with none.
//
// Run from this directory:
//   swift generate.swift
import CoreGraphics
import CoreText
import Foundation
import ImageIO
import UniformTypeIdentifiers

let directory = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
let latin = directory.appendingPathComponent("latin.png")
let mixed = directory.appendingPathComponent("mixed.pdf")

/// Draws one line of black Helvetica text with its baseline at `origin`.
func drawText(_ context: CGContext, _ text: String, at origin: CGPoint, size: CGFloat) {
    let font = CTFontCreateWithName("Helvetica" as CFString, size, nil)
    let attributes: [NSAttributedString.Key: Any] = [
        NSAttributedString.Key(kCTFontAttributeName as String): font,
        NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(gray: 0, alpha: 1),
    ]
    let line = CTLineCreateWithAttributedString(
        NSAttributedString(string: text, attributes: attributes)
    )
    context.textPosition = origin
    CTLineDraw(line, context)
}

/// A white RGB bitmap of `width` × `height` pixels.
func bitmap(width: Int, height: Int) -> CGContext {
    let space = CGColorSpaceCreateDeviceRGB()
    let context = CGContext(
        data: nil,
        width: width,
        height: height,
        bitsPerComponent: 8,
        bytesPerRow: 0,
        space: space,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    )!
    context.setFillColor(CGColor(gray: 1, alpha: 1))
    context.fill(CGRect(x: 0, y: 0, width: width, height: height))
    return context
}

/// Writes `image` as PNG at `url`.
func writePNG(_ image: CGImage, to url: URL) {
    let destination = CGImageDestinationCreateWithURL(
        url as CFURL, UTType.png.identifier as CFString, 1, nil
    )!
    CGImageDestinationAddImage(destination, image, nil)
    precondition(CGImageDestinationFinalize(destination), "latin.png could not be written")
}

// latin.png: three lines the engine reads back.
let latinContext = bitmap(width: 1200, height: 380)
drawText(latinContext, "DIMCODE OCR", at: CGPoint(x: 120, y: 260), size: 72)
drawText(latinContext, "12345678", at: CGPoint(x: 120, y: 140), size: 72)
drawText(latinContext, "apple vision", at: CGPoint(x: 120, y: 40), size: 48)
let latinImage = latinContext.makeImage()!
writePNG(latinImage, to: latin)

// mixed.pdf: page one with a text layer, page two with the image instead.
var mediaBox = CGRect(x: 0, y: 0, width: 612, height: 792)
let pdf = CGContext(mixed as CFURL, mediaBox: &mediaBox, nil)!

pdf.beginPDFPage(nil)
drawText(pdf, "Native page one", at: CGPoint(x: 72, y: 640), size: 40)
drawText(pdf, "drawn as a text layer", at: CGPoint(x: 72, y: 580), size: 28)
pdf.endPDFPage()

pdf.beginPDFPage(nil)
pdf.draw(latinImage, in: CGRect(x: 72, y: 392, width: 468, height: 148))
pdf.endPDFPage()
pdf.closePDF()

// scanned.pdf: two pages that are images, with no text layer of their own.
let scanned = directory.appendingPathComponent("scanned.pdf")
let scannedPdf = CGContext(scanned as CFURL, mediaBox: &mediaBox, nil)!
for _ in 0..<2 {
    scannedPdf.beginPDFPage(nil)
    scannedPdf.draw(latinImage, in: CGRect(x: 72, y: 322, width: 468, height: 148))
    scannedPdf.endPDFPage()
}
scannedPdf.closePDF()
