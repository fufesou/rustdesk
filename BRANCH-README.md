Title: S1 – macOS prefer PNG when publishing images

Problem
Windows → macOS pasting sometimes shows white background/halo on images. A common cause on macOS is that many apps consume TIFF from the pasteboard by default, and TIFF produced via NSImage can expose alpha/premultiplication differences. Ensuring a PNG representation is present on the pasteboard generally preserves transparency.

Scope of this branch
- Only affects macOS image write path via arboard. No changes to other platforms.
- Minimal change: when the source image is RGBA, publish PNG bytes to the pasteboard instead of relying on NSImage (which typically exposes TIFF by default).
- Item model is intentionally kept as-is (no change to how many items are written). This branch does not attempt to merge multiple items.

Implementation (in arboard)
- File: src/platform/osx.rs
- In Set::formats, for ClipboardData::Image(ImageData::Rgba), convert RGBA→NSImage, then transcode to PNG using NSBitmapImageRep (representationUsingType:NSBitmapImageFileTypePNG). Write NSPasteboardTypePNG.
- Fallback: if transcoding fails, fall back to the previous behavior of writing NSImage.
- Logging: all added logs are prefixed with ====================== for easy grepping.

Build & test
- Windows: cargo check works (macOS-only code is behind target_os = "macos").
- macOS: build arboard and rustdesk normally.
- Manual test: from Windows, use Win+Shift+S, paste into Notes / Messages / Pages on macOS. Expect transparency preserved.

Inspect pasteboard types on macOS
- Use the tiny Swift tool below to list pasteboard items and types.

Swift helper (paste as PasteboardDump.swift and run with `swiftc PasteboardDump.swift -o pb && ./pb`)

import AppKit
let pb = NSPasteboard.general
for (idx,item) in (pb.pasteboardItems ?? []).enumerated() {
    print("Item #\(idx):")
    for t in item.types { print("  type: \(t.rawValue)") }
}

Known limitations
- This does not eliminate duplicate items. Some apps may still paste multiple images if multiple items exist.
- SVG input is not rasterized.

Next steps (other branches)
- S2: write-time dedupe (prefer PNG over RGBA when both exist).
- S3: single NSPasteboardItem with multiple representations (PNG + TIFF + optional SVG), with env switch to disable TIFF.
- S4: Windows reader: prefer CF_PNG; handle CF_DIBV5/CF_DIB and alpha defaults.
