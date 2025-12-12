Title: S4 – Windows reader: prefer CF_PNG; handle CF_DIBV5/CF_DIB and alpha defaults

Problem
Some Windows screenshots expose undefined alpha (BI_RGB 32bpp) or format variance. When pasting to macOS, misinterpreted alpha can manifest as white boxes/halos.

Scope (arboard/windows)
- Prefer CF_PNG when available.
- Fall back to CF_DIBV5 as today.
- Then CF_DIB (BI_RGB 32bpp). For BI_RGB 32, treat alpha as undefined and force A=0xFF.
- Add debug logs with the `======================` prefix.

Implementation (in arboard)
- File: src/platform/windows.rs
- Get::image_(): order becomes SVG → PNG → DIBV5 → DIB.
- New reader for CF_DIB that constructs an HBITMAP via CreateDIBitmap and converts to RGBA; sets alpha to 255.
- formats(): for ClipboardFormat::ImageRgba, attempt DIBV5 then DIB.

Build & test
- Build and run on Windows.
- Verify debug logs show which Windows format was chosen.
- Manual test: Win+Shift+S → paste to macOS; expect white/transparent regions to look correct when combined with S1/S3.

Notes
- No Cargo.toml changes.
