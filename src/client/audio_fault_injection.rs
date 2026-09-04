use hbb_common::log;

const ENABLE_ENV: &str = "RUSTDESK_TEST_AUDIO_DROP";
const ACTIVE_SAMPLE_THRESHOLD: f32 = 0.01;
const ACTIVE_PACKETS_PER_PERIOD: usize = 100;
const DROP_START_PACKET: usize = 50;
const DROP_PACKET_COUNT: usize = 5;
const DROP_END_PACKET: usize = DROP_START_PACKET + DROP_PACKET_COUNT;
const PACKET_DURATION_MS: usize = 10;

pub(super) struct AudioFaultInjection {
    enabled: bool,
    active_packet_index: usize,
}

impl Default for AudioFaultInjection {
    fn default() -> Self {
        let enabled = std::env::var_os(ENABLE_ENV).is_some();
        if enabled {
            log::warn!(
                "Audio fault injection enabled: dropping {} ms of active audio every second",
                DROP_PACKET_COUNT * PACKET_DURATION_MS
            );
        }
        Self::new(enabled)
    }
}

impl AudioFaultInjection {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            active_packet_index: 0,
        }
    }

    #[cfg(test)]
    fn for_test() -> Self {
        Self::new(true)
    }

    pub(super) fn should_drop(&mut self, samples: &[f32]) -> bool {
        if !self.enabled
            || !samples
                .iter()
                .any(|sample| sample.abs() >= ACTIVE_SAMPLE_THRESHOLD)
        {
            return false;
        }

        let period_index = self.active_packet_index % ACTIVE_PACKETS_PER_PERIOD;
        self.active_packet_index = self.active_packet_index.wrapping_add(1);
        if period_index == DROP_START_PACKET {
            log::warn!(
                "Audio fault injection: dropping the next {} decoded packets ({} ms)",
                DROP_PACKET_COUNT,
                DROP_PACKET_COUNT * PACKET_DURATION_MS
            );
        }
        (DROP_START_PACKET..DROP_END_PACKET).contains(&period_index)
    }
}

#[cfg(test)]
mod tests {
    use super::AudioFaultInjection;

    const ACTIVE_SAMPLES: [f32; 1] = [0.25];
    const SILENT_SAMPLES: [f32; 1] = [0.0];

    #[test]
    fn disabled_injection_never_drops_audio() {
        let mut injection = AudioFaultInjection::new(false);

        for _ in 0..200 {
            assert!(!injection.should_drop(&ACTIVE_SAMPLES));
        }
    }

    #[test]
    fn drops_five_active_packets_after_half_second_each_period() {
        let mut injection = AudioFaultInjection::for_test();

        let dropped: Vec<usize> = (0_usize..200)
            .filter(|_| injection.should_drop(&ACTIVE_SAMPLES))
            .collect();

        assert_eq!(dropped, vec![50, 51, 52, 53, 54, 150, 151, 152, 153, 154]);
    }

    #[test]
    fn silent_packets_do_not_advance_the_schedule() {
        let mut injection = AudioFaultInjection::for_test();

        for _ in 0..100 {
            assert!(!injection.should_drop(&SILENT_SAMPLES));
        }

        let dropped: Vec<usize> = (0_usize..100)
            .filter(|_| injection.should_drop(&ACTIVE_SAMPLES))
            .collect();

        assert_eq!(dropped, vec![50, 51, 52, 53, 54]);
    }
}
