use super::*;

const LOGICAL_BITRATE_KBPS: u32 = 2_543;
const RATE_CONTROL_FPS: u32 = 10;

#[test]
fn qsv_hardware_ram_encoder_supports_fps_aware_rate_control() {
    assert!(supports_fps_aware_rate_control("hevc_qsv"));
    assert!(!supports_fps_aware_rate_control("h264_vaapi"));
}

#[test]
fn lower_rate_control_fps_preserves_hardware_ram_frame_budget() {
    let configured_bitrate =
        scale_bitrate_for_rate_control(LOGICAL_BITRATE_KBPS, DEFAULT_FPS as u32, RATE_CONTROL_FPS)
            .unwrap();

    assert_eq!(configured_bitrate, 7_629);
    assert_eq!(
        configured_bitrate * RATE_CONTROL_FPS,
        LOGICAL_BITRATE_KBPS * DEFAULT_FPS as u32
    );
}

#[test]
fn zero_hardware_ram_frame_budget_is_rejected() {
    assert!(scale_bitrate_for_rate_control(LOGICAL_BITRATE_KBPS, DEFAULT_FPS as u32, 0).is_err());
}
