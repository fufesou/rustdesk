use super::*;

const DISPLAY_NAME: &str = "monitor0";
const CONNECTION_ID: i32 = 1;
const QUALITY_TIMESTAMP: i64 = 1;
const HIGH_DELAY_MS: u32 = 400;
const LOW_RATIO: f32 = 0.1;
const TARGET_RATIO: f32 = 1.0;
const STATIC_SEND_COUNT: usize = 0;
const DYNAMIC_SEND_COUNT: usize = 6;
const SETTLE_CHANGE_SEND_COUNT: usize = 3;
const REFRESH_FIRST_FRAME_SEND_COUNT: usize = 1;

fn qos_with_high_delay(initial_ratio: f32) -> VideoQoS {
    let mut qos = VideoQoS::default();
    qos.ratio = initial_ratio;
    qos.users.insert(
        CONNECTION_ID,
        UserData {
            quality: Some((QUALITY_TIMESTAMP, Quality::Custom(TARGET_RATIO))),
            delay: UserDelay {
                delay_history: VecDeque::from([HIGH_DELAY_MS]),
                ..UserDelay::default()
            },
            ..UserData::default()
        },
    );
    qos.new_display(DISPLAY_NAME.to_owned());
    qos.set_support_changing_quality(DISPLAY_NAME, true);
    qos
}

#[test]
fn static_screen_restores_target_quality_during_high_delay() {
    let mut qos = qos_with_high_delay(LOW_RATIO);

    qos.adjust_ratio(false, STATIC_SEND_COUNT);

    assert_eq!(qos.ratio(), TARGET_RATIO);
    assert_eq!(qos.take_static_refresh(DISPLAY_NAME), Some(true));
    assert_eq!(qos.take_static_refresh(DISPLAY_NAME), Some(false));
}

#[test]
fn static_screen_at_target_quality_does_not_request_refresh() {
    let mut qos = qos_with_high_delay(TARGET_RATIO);

    qos.adjust_ratio(false, STATIC_SEND_COUNT);

    assert_eq!(qos.ratio(), TARGET_RATIO);
    assert_eq!(qos.take_static_refresh(DISPLAY_NAME), Some(false));
}

#[test]
fn settled_screen_at_target_quality_requests_one_refresh() {
    let mut qos = qos_with_high_delay(TARGET_RATIO);

    qos.adjust_ratio(false, SETTLE_CHANGE_SEND_COUNT);
    assert_eq!(qos.take_static_refresh(DISPLAY_NAME), Some(false));

    qos.adjust_ratio(false, STATIC_SEND_COUNT);
    assert_eq!(qos.take_static_refresh(DISPLAY_NAME), Some(true));
    assert_eq!(qos.take_static_refresh(DISPLAY_NAME), Some(false));

    qos.adjust_ratio(false, REFRESH_FIRST_FRAME_SEND_COUNT);
    qos.adjust_ratio(false, STATIC_SEND_COUNT);
    assert_eq!(qos.take_static_refresh(DISPLAY_NAME), Some(false));
}

#[test]
fn dynamic_screen_still_reduces_quality_during_high_delay() {
    let mut qos = qos_with_high_delay(TARGET_RATIO);

    qos.adjust_ratio(true, DYNAMIC_SEND_COUNT);

    assert!(qos.ratio() < TARGET_RATIO);
    assert_eq!(qos.take_static_refresh(DISPLAY_NAME), Some(false));
}
