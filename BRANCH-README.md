Title: S3 – macOS single item with multiple image representations (PNG + TIFF + optional SVG)

Problem
Multiple pasteboard items can be pasted as multiple images by some macOS apps. Best practice is to publish one NSPasteboardItem with multiple representations, letting the consumer choose.

Scope
- macOS-only change in arboard: write a single NSPasteboardItem for an image and attach multiple types: public.png (always) + public.tiff (unless disabled) + SVG (if present).
- Adds an environment variable toggle to disable TIFF generation at runtime: RUSTDESK_PB_NO_TIFF=1.

Implementation (in arboard)
- File: src/platform/osx.rs
- In Set::formats, aggregate image data across ClipboardData entries into one NSPasteboardItem. Always set NSPasteboardTypePNG. Unless RUSTDESK_PB_NO_TIFF is set, also attach NSPasteboardTypeTIFF.
- If the input is RGBA, construct NSImage from pixels and transcode to PNG (and TIFF). If input is PNG, reuse it; TIFF is derived via NSBitmapImageRep. If input is SVG, we attach SVG text; no rasterization is performed in this branch.
- All new logs prefixed with ======================

Build & test
- Windows: cargo check (macOS code is cfg-gated).
- macOS: paste screenshots into various apps. Expect a single item with multiple types. Use the Swift pasteboard dumper to verify types.

Swift helper (PasteboardDump.swift)
import AppKit
let pb = NSPasteboard.general
for (idx,item) in (pb.pasteboardItems ?? []).enumerated() {
    print("Item #\(idx):")
    for t in item.types { print("  type: \(t.rawValue)") }
}

Notes
- No Cargo.toml changes; uses AppKit APIs (NSBitmapImageRep) for transcoding.
- TIFF can be disabled at runtime with RUSTDESK_PB_NO_TIFF=1 to compare behavior/perf.
