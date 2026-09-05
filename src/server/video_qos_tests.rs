use super::*;

const DISPLAY_NAME: &str = "monitor0";
const CONNECTION_ID: i32 = 1;
const QUALITY_TIMESTAMP: i64 = 1;
const HIGH_DELAY_MS: u32 = 400;
const LOW_RATIO: f32 = 0.1;
const TARGET_RATIO: f32 = 1.0;
const STATIC_SEND_COUNT: usize = 0;
const DYNAMIC_SEND_COUNT: usize = 6;
const POST_REFRESH_FRAME_SEND_COUNT: usize = 3;
const BASE_RTT_MS: u32 = 400;
const STEADY_HIGH_RTT_DELAY_MS: u32 = 420;
const RTT_LEARNING_SAMPLES: usize = 10;
const FPS_RECOVERY_SAMPLES: usize = 9;
const BALANCED_CLARITY_FPS: u32 = 10;
const CUSTOM_CLARITY_FPS: u32 = 6;

fn qos_with_high_delay(initial_ratio: f32) -> VideoQoS {
    let mut qos = VideoQoS::default();
    qos.clarity_fps_override = None;
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

fn qos_with_balanced_high_delay(initial_ratio: f32) -> VideoQoS {
    let mut qos = qos_with_high_delay(initial_ratio);
    qos.users.get_mut(&CONNECTION_ID).unwrap().quality =
        Some((QUALITY_TIMESTAMP, Quality::Balanced));
    qos
}

#[test]
fn static_screen_restores_target_quality_during_high_delay() {
    let mut qos = qos_with_high_delay(LOW_RATIO);

    qos.adjust_ratio(false, STATIC_SEND_COUNT);

    assert_eq!(qos.ratio(), TARGET_RATIO);
    assert_eq!(
        qos.take_static_refresh(DISPLAY_NAME),
        Some(StaticRefresh::Quality)
    );
    assert_eq!(
        qos.take_static_refresh(DISPLAY_NAME),
        Some(StaticRefresh::None)
    );
}

#[test]
fn static_screen_at_target_quality_does_not_request_refresh() {
    let mut qos = qos_with_high_delay(TARGET_RATIO);

    qos.adjust_ratio(false, STATIC_SEND_COUNT);

    assert_eq!(qos.ratio(), TARGET_RATIO);
    assert_eq!(
        qos.take_static_refresh(DISPLAY_NAME),
        Some(StaticRefresh::None)
    );
}

#[test]
fn settled_screen_at_target_quality_requests_one_refresh() {
    let mut qos = qos_with_high_delay(TARGET_RATIO);

    qos.adjust_ratio(true, DYNAMIC_SEND_COUNT);
    assert_eq!(
        qos.take_static_refresh(DISPLAY_NAME),
        Some(StaticRefresh::None)
    );

    qos.adjust_ratio(false, STATIC_SEND_COUNT);
    assert_eq!(
        qos.take_static_refresh(DISPLAY_NAME),
        Some(StaticRefresh::Settled)
    );
    assert_eq!(
        qos.take_static_refresh(DISPLAY_NAME),
        Some(StaticRefresh::None)
    );

    qos.adjust_ratio(false, POST_REFRESH_FRAME_SEND_COUNT);
    qos.adjust_ratio(false, STATIC_SEND_COUNT);
    assert_eq!(
        qos.take_static_refresh(DISPLAY_NAME),
        Some(StaticRefresh::None)
    );
}

#[test]
fn high_delay_reduces_fps_before_dynamic_quality() {
    let mut qos = qos_with_balanced_high_delay(BR_BALANCED);
    let initial_fps = qos.fps();

    qos.user_network_delay(CONNECTION_ID, HIGH_DELAY_MS);
    qos.adjust_ratio(true, DYNAMIC_SEND_COUNT);

    assert!(qos.fps() < initial_fps);
    assert_eq!(qos.ratio(), BR_BALANCED);
    assert_eq!(
        qos.take_static_refresh(DISPLAY_NAME),
        Some(StaticRefresh::None)
    );
}

#[test]
fn dynamic_screen_reduces_quality_after_fps_reaches_minimum() {
    let mut qos = qos_with_high_delay(TARGET_RATIO);
    qos.fps = MIN_FPS;

    qos.adjust_ratio(true, DYNAMIC_SEND_COUNT);

    assert!(qos.ratio() < TARGET_RATIO);
    assert_eq!(
        qos.take_static_refresh(DISPLAY_NAME),
        Some(StaticRefresh::None)
    );
}

#[test]
fn repeated_dynamic_high_delay_respects_balanced_quality_floor() {
    let mut qos = qos_with_balanced_high_delay(BR_BALANCED);
    qos.fps = MIN_FPS;

    for _ in 0..10 {
        qos.adjust_ratio(true, DYNAMIC_SEND_COUNT);
    }

    assert_eq!(qos.ratio(), BR_SPEED);
    assert_eq!(
        qos.take_static_refresh(DISPLAY_NAME),
        Some(StaticRefresh::None)
    );
}

#[test]
fn dynamic_high_rtt_does_not_restore_fps_above_clarity_floor() {
    let mut qos = qos_with_balanced_high_delay(BR_BALANCED);
    let user = qos.users.get_mut(&CONNECTION_ID).unwrap();
    for _ in 0..RTT_LEARNING_SAMPLES {
        user.delay.add_delay(BASE_RTT_MS);
    }
    user.delay.delay_history = VecDeque::from([STEADY_HIGH_RTT_DELAY_MS]);
    user.delay.fps = Some(BALANCED_CLARITY_FPS);
    qos.fps = BALANCED_CLARITY_FPS;
    qos.update_display_data(DISPLAY_NAME, DYNAMIC_SEND_COUNT);

    for _ in 0..FPS_RECOVERY_SAMPLES {
        qos.user_network_delay(CONNECTION_ID, STEADY_HIGH_RTT_DELAY_MS);
    }

    assert!(qos.fps() <= BALANCED_CLARITY_FPS);

    qos.update_display_data(DISPLAY_NAME, STATIC_SEND_COUNT);
    for _ in 0..FPS_RECOVERY_SAMPLES {
        qos.user_network_delay(CONNECTION_ID, STEADY_HIGH_RTT_DELAY_MS);
    }

    assert!(qos.fps() > BALANCED_CLARITY_FPS);
}

#[test]
fn encoder_frame_budget_stops_at_balanced_clarity_floor() {
    let mut qos = qos_with_balanced_high_delay(BR_BALANCED);
    qos.fps = MIN_FPS;

    assert_eq!(qos.encoder_frame_budget_fps(), BALANCED_CLARITY_FPS);
}

#[test]
fn clarity_fps_parser_accepts_only_supported_values() {
    assert_eq!(parse_clarity_fps("6").unwrap(), CUSTOM_CLARITY_FPS);
    assert_eq!(parse_clarity_fps("1").unwrap(), MIN_FPS);
    assert_eq!(parse_clarity_fps("30").unwrap(), FPS);
    assert!(parse_clarity_fps("0").is_err());
    assert!(parse_clarity_fps("31").is_err());
    assert!(parse_clarity_fps("invalid").is_err());
}

#[test]
fn clarity_fps_override_caps_high_rtt_motion_and_frame_budget() {
    let mut qos = qos_with_balanced_high_delay(BR_BALANCED);
    qos.clarity_fps_override = Some(CUSTOM_CLARITY_FPS);
    let user = qos.users.get_mut(&CONNECTION_ID).unwrap();
    for _ in 0..RTT_LEARNING_SAMPLES {
        user.delay.add_delay(BASE_RTT_MS);
    }
    user.delay.delay_history = VecDeque::from([STEADY_HIGH_RTT_DELAY_MS]);
    user.delay.fps = Some(BALANCED_CLARITY_FPS);
    qos.fps = BALANCED_CLARITY_FPS;
    qos.update_display_data(DISPLAY_NAME, DYNAMIC_SEND_COUNT);

    qos.user_network_delay(CONNECTION_ID, STEADY_HIGH_RTT_DELAY_MS);

    assert_eq!(qos.fps(), CUSTOM_CLARITY_FPS);
    assert_eq!(qos.encoder_frame_budget_fps(), CUSTOM_CLARITY_FPS);
}

#[test]
fn clarity_fps_override_is_held_until_settled_refresh() {
    let mut qos = qos_with_balanced_high_delay(BR_BALANCED);
    qos.clarity_fps_override = Some(CUSTOM_CLARITY_FPS);
    let user = qos.users.get_mut(&CONNECTION_ID).unwrap();
    for _ in 0..RTT_LEARNING_SAMPLES {
        user.delay.add_delay(BASE_RTT_MS);
    }
    user.delay.delay_history = VecDeque::from([STEADY_HIGH_RTT_DELAY_MS]);
    user.delay.fps = Some(BALANCED_CLARITY_FPS);
    qos.fps = BALANCED_CLARITY_FPS;
    qos.settle_refresh_armed = true;
    qos.update_display_data(DISPLAY_NAME, STATIC_SEND_COUNT);

    qos.user_network_delay(CONNECTION_ID, STEADY_HIGH_RTT_DELAY_MS);

    assert_eq!(qos.fps(), CUSTOM_CLARITY_FPS);
    qos.adjust_ratio(false, STATIC_SEND_COUNT);
    assert_eq!(
        qos.take_static_refresh(DISPLAY_NAME),
        Some(StaticRefresh::Settled)
    );

    qos.user_network_delay(CONNECTION_ID, STEADY_HIGH_RTT_DELAY_MS);

    assert!(qos.fps() > CUSTOM_CLARITY_FPS);
}
