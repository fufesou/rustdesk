Title: S2 – macOS write-time dedupe: prefer PNG over RGBA

Problem
Some macOS apps treat multiple pasteboard items as multiple images to paste. If we publish both RGBA and PNG as separate items, duplication or wrong representation selection can occur.

Scope of this branch
- macOS-only behavior in arboard: when both PNG and RGBA forms of the same image are present to be published, publish only PNG.
- No changes for other platforms.

Implementation (in arboard)
- File: src/platform/osx.rs
- In Set::formats, scan data for any ImageData::Png. If present, skip ImageData::Rgba entries.
- Add logs with the signature prefix ====================== when skipping RGBA.

Build & test
- Windows: cargo check (macOS-only code behind target_os = "macos").
- macOS: run RustDesk and paste Windows screenshots; inspect pasteboard items with the Swift helper (one image item should be present).

Swift helper (PasteboardDump.swift)
import AppKit
let pb = NSPasteboard.general
for (idx,item) in (pb.pasteboardItems ?? []).enumerated() {
    print("Item #\(idx):")
    for t in item.types { print("  type: \(t.rawValue)") }
}

Notes
- This is minimal and isolated to macOS write path. It does not change item merging (see S3 for that experiment).
