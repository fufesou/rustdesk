use hbb_common::{anyhow::anyhow, ResultType};
use std::{
    sync::{Mutex, TryLockError},
    time::{Duration, Instant},
};

const RETRY_INTERVAL: Duration = Duration::from_secs(5);

pub(super) fn poll() -> bool {
    static LAST_FAILURE: Mutex<Option<Instant>> = Mutex::new(None);
    match try_poll(
        &LAST_FAILURE,
        crate::virtual_display_manager::plug_in_headless,
    ) {
        Ok(attached) => attached,
        Err(error) => {
            hbb_common::throttled_log!(
                RETRY_INTERVAL,
                error,
                "Cannot create a background headless display: {error:#}"
            );
            false
        }
    }
}

fn try_poll(
    last_failure: &Mutex<Option<Instant>>,
    plug_in: impl FnOnce() -> ResultType<()>,
) -> ResultType<bool> {
    let mut last_failure = match last_failure.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => return Ok(false),
        Err(error) => {
            return Err(anyhow!(
                "Cannot acquire the headless display retry lock: {error}"
            ))
        }
    };
    if last_failure.is_some_and(|at| at.elapsed() < RETRY_INTERVAL) {
        return Ok(false);
    }
    let result = plug_in();
    *last_failure = result.is_err().then(Instant::now);
    result.map(|()| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_poll_waits_after_a_failure_and_retries_after_the_interval() {
        let last_failure = Mutex::new(None);
        assert!(try_poll(&last_failure, || Err(anyhow!("install failed"))).is_err());
        assert!(!try_poll(&last_failure, || panic!("retried too soon")).unwrap());

        let expired = Mutex::new(Instant::now().checked_sub(RETRY_INTERVAL));
        assert!(try_poll(&expired, || Ok(())).unwrap());
        assert!(try_poll(&expired, || Ok(())).unwrap());
    }
}
