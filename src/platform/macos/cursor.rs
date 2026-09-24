use super::{CursorData, ResultType};
use cocoa::{
    appkit::NSCompositingOperation,
    base::{id, nil, NO, YES},
    foundation::{NSInteger, NSPoint, NSRect, NSSize, NSString},
};
use hbb_common::{anyhow::Context, bail};
use objc::{class, msg_send, rc::StrongPtr, sel, sel_impl};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    ptr, slice,
};

const CHANNELS: usize = 4;
const BITS_PER_SAMPLE: NSInteger = 8;

pub(super) fn scale() -> ResultType<f64> {
    if !*scrap::quartz::ENABLE_RETINA.lock().unwrap() {
        return Ok(1.0);
    }
    unsafe {
        let point: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let screens: id = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![screens, count];
        for index in 0..count {
            let screen: id = msg_send![screens, objectAtIndex: index];
            let frame: NSRect = msg_send![screen, frame];
            // AppKit's bottom-left coordinates include the upper screen edge.
            if point.x >= frame.origin.x
                && point.y > frame.origin.y
                && point.x < frame.origin.x + frame.size.width
                && point.y <= frame.origin.y + frame.size.height
            {
                return Ok(msg_send![screen, backingScaleFactor]);
            }
        }
    }
    bail!("No macOS display contains the cursor")
}

pub(super) fn cache_id(cursor: u64, scale: f64) -> u64 {
    // Legacy Web decoders require JS-safe integers; zero is the service's initial ID.
    const MAX_CURSOR_ID: u64 = (1 << 53) - 1;
    let mut hash = DefaultHasher::new();
    (cursor, scale.to_bits()).hash(&mut hash);
    hash.finish() % MAX_CURSOR_ID + 1
}

unsafe fn bitmap(size: NSSize) -> ResultType<StrongPtr> {
    let color_space = StrongPtr::new(NSString::alloc(nil).init_str("NSDeviceRGBColorSpace"));
    let bitmap: id = msg_send![class!(NSBitmapImageRep), alloc];
    let bitmap: id = msg_send![bitmap,
        initWithBitmapDataPlanes: ptr::null_mut::<*mut u8>()
        pixelsWide: size.width as NSInteger pixelsHigh: size.height as NSInteger
        bitsPerSample: BITS_PER_SAMPLE samplesPerPixel: CHANNELS as NSInteger
        hasAlpha: YES isPlanar: NO colorSpaceName: *color_space
        bitmapFormat: 0usize bytesPerRow: (size.width as usize * CHANNELS) as NSInteger
        bitsPerPixel: BITS_PER_SAMPLE * CHANNELS as NSInteger];
    if bitmap == nil {
        bail!("Could not allocate the macOS cursor bitmap");
    }
    Ok(StrongPtr::new(bitmap))
}

unsafe fn render(image: id, bitmap: id, size: NSSize) -> ResultType<()> {
    let context: id =
        msg_send![class!(NSGraphicsContext), graphicsContextWithBitmapImageRep: bitmap];
    if context == nil {
        bail!("Could not create the macOS cursor graphics context");
    }
    let (): () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
    let (): () = msg_send![class!(NSGraphicsContext), setCurrentContext: context];
    // Drawing at the pixel size lets AppKit select the matching image representation.
    let (): () = msg_send![image,
        drawInRect: NSRect::new(NSPoint::new(0.0, 0.0), size)
        fromRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0))
        operation: NSCompositingOperation::NSCompositeCopy fraction: 1.0f64];
    let (): () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];
    Ok(())
}

pub(super) unsafe fn data(cursor: id, id: u64, scale: f64) -> ResultType<CursorData> {
    // Older receivers ignore density. Keep their image logical-sized while
    // retaining the complete physical artwork for density-aware controllers.
    let mut legacy = physical_data(cursor, id, 1.0)?;
    if scale > 1.0 {
        legacy.high_resolution = Some(physical_data(cursor, id, scale)?).into();
    }
    Ok(legacy)
}

unsafe fn physical_data(cursor: id, id: u64, scale: f64) -> ResultType<CursorData> {
    let image: id = msg_send![cursor, image];
    let logical: NSSize = msg_send![image, size];
    let size = NSSize::new(
        (logical.width * scale).round(),
        (logical.height * scale).round(),
    );
    if !size.width.is_finite()
        || !size.height.is_finite()
        || size.width <= 0.0
        || size.height <= 0.0
        || size.width > i32::MAX as f64
        || size.height > i32::MAX as f64
    {
        bail!("Invalid macOS cursor dimensions");
    }
    let length = (size.width as usize)
        .checked_mul(size.height as usize)
        .and_then(|pixels| pixels.checked_mul(CHANNELS))
        .context("Cursor bitmap size overflow")?;
    let bitmap = bitmap(size)?;
    render(image, *bitmap, size)?;
    let pixels: *const u8 = msg_send![*bitmap, bitmapData];
    if pixels.is_null() {
        bail!("Could not read the macOS cursor bitmap");
    }
    let hotspot: NSPoint = msg_send![cursor, hotSpot];
    Ok(CursorData {
        id,
        colors: straight_rgba(slice::from_raw_parts(pixels, length)).into(),
        // A valid fractional hotspot near an edge can round past the last pixel.
        hotx: (hotspot.x * size.width / logical.width)
            .round()
            .min(size.width - 1.0) as _,
        hoty: (hotspot.y * size.height / logical.height)
            .round()
            .min(size.height - 1.0) as _,
        width: size.width as _,
        height: size.height as _,
        scale,
        ..Default::default()
    })
}

fn straight_rgba(pixels: &[u8]) -> Vec<u8> {
    // AppKit renders premultiplied pixels, but macOS cursor packets have always
    // used straight alpha. Density metadata does not negotiate a new format.
    const MAX_CHANNEL: u16 = u8::MAX as u16;
    let mut colors = pixels.to_vec();
    for pixel in colors.chunks_exact_mut(CHANNELS) {
        let alpha = u16::from(pixel[CHANNELS - 1]);
        if alpha == 0 {
            continue;
        }
        for channel in &mut pixel[..CHANNELS - 1] {
            *channel =
                ((u16::from(*channel) * MAX_CHANNEL + alpha / 2) / alpha).min(MAX_CHANNEL) as u8;
        }
    }
    colors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_keeps_straight_alpha_for_legacy_receivers() {
        for (premultiplied, straight) in [
            ([128, 128, 128, 128], [255, 255, 255, 128]),
            ([64, 32, 16, 128], [128, 64, 32, 128]),
            ([240, 100, 20, 255], [240, 100, 20, 255]),
            ([0, 0, 0, 0], [0, 0, 0, 0]),
        ] {
            assert_eq!(straight_rgba(&premultiplied), straight);
        }
    }
}
