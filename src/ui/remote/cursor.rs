use base::message_proto::CursorData;

const CHANNELS: usize = 4;
const ALPHA: usize = CHANNELS - 1;
const MAX_CHANNEL: u16 = u8::MAX as u16;

pub(super) fn png_colors(cursor: &CursorData, is_macos: bool) -> Vec<u8> {
    let mut colors = hbb_common::compress::decompress(&cursor.colors);
    // New macOS packets are premultiplied for Flutter, but PNG stores straight
    // colors. Legacy macOS packets (scale zero) already contain straight RGBA.
    if !(is_macos && cursor.scale > 0.0) {
        return colors;
    }
    for pixel in colors.chunks_exact_mut(CHANNELS) {
        let alpha = u16::from(pixel[ALPHA]);
        if alpha == 0 {
            continue;
        }
        for channel in &mut pixel[..ALPHA] {
            // Capture can round RGB and alpha differently by one channel value.
            *channel =
                ((u16::from(*channel) * MAX_CHANNEL + alpha / 2) / alpha).min(MAX_CHANNEL) as u8;
        }
    }
    colors
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRAIGHT: [u8; 16] = [
        255, 255, 255, 128, 128, 64, 32, 128, 240, 100, 20, 255, 0, 0, 0, 0,
    ];
    const PREMULTIPLIED: [u8; 16] = [
        128, 128, 128, 128, 64, 32, 16, 128, 240, 100, 20, 255, 0, 0, 0, 0,
    ];

    fn encode_packet(pixels: &[u8], scale: f64, is_macos: bool) -> Vec<u8> {
        let cursor = CursorData {
            width: 4,
            height: 1,
            colors: hbb_common::compress::compress(pixels).into(),
            scale,
            ..Default::default()
        };
        let colors = png_colors(&cursor, is_macos);
        let mut png = Vec::new();
        repng::encode(&mut png, cursor.width as _, cursor.height as _, &colors).unwrap();
        assert_eq!(hbb_common::compress::decompress(&cursor.colors), pixels);
        image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8()
            .into_raw()
    }

    #[test]
    fn macos_density_packets_encode_straight_png_colors() {
        for scale in [1.0, 2.0] {
            let actual = encode_packet(&PREMULTIPLIED, scale, true);
            for (got, expected) in actual
                .chunks_exact(CHANNELS)
                .zip(STRAIGHT.chunks_exact(CHANNELS))
            {
                assert_eq!(got[ALPHA], expected[ALPHA]);
                for channel in 0..ALPHA {
                    assert!(got[channel].abs_diff(expected[channel]) <= 1, "{actual:?}");
                }
            }
        }
    }

    #[test]
    fn legacy_and_non_macos_packets_keep_their_colors() {
        for (scale, is_macos) in [(0.0, true), (0.0, false), (2.0, false)] {
            assert_eq!(encode_packet(&STRAIGHT, scale, is_macos), STRAIGHT);
        }
    }
}
