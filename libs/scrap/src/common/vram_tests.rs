use super::*;

const LOGICAL_BITRATE_KBPS: u32 = 2_543;
const NOMINAL_FPS: u32 = 30;
const RATE_CONTROL_FPS: u32 = 10;

#[test]
fn lower_rate_control_fps_preserves_logical_bandwidth_per_frame() {
    let configured_bitrate =
        scale_bitrate_for_rate_control(LOGICAL_BITRATE_KBPS, NOMINAL_FPS, RATE_CONTROL_FPS)
            .unwrap();

    assert_eq!(configured_bitrate, 7_629);
    assert_eq!(
        configured_bitrate * RATE_CONTROL_FPS,
        LOGICAL_BITRATE_KBPS * NOMINAL_FPS
    );
}

#[test]
fn zero_rate_control_fps_is_rejected() {
    assert!(scale_bitrate_for_rate_control(LOGICAL_BITRATE_KBPS, NOMINAL_FPS, 0).is_err());
    assert!(scale_bitrate_for_rate_control(LOGICAL_BITRATE_KBPS, 0, RATE_CONTROL_FPS).is_err());
}

#[test]
fn configured_bitrate_overflow_is_rejected() {
    assert!(scale_bitrate_for_rate_control(u32::MAX, u32::MAX, 1).is_err());
}
