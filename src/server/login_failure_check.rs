use crate::AlarmAuditType;
use hbb_common::{
    get_time, log,
    tokio::sync::{Mutex as TokioMutex, OwnedMutexGuard},
};
use std::sync::{Arc, Mutex};

const TERMINAL_OS_LOGIN_TOTAL_IDLE_RESET_MS: i64 = 120 * 60 * 1_000;
const TERMINAL_OS_LOGIN_BACKOFF_BASE_SECONDS: i64 = 15;
const TERMINAL_OS_LOGIN_BACKOFF_MAX_SECONDS: i64 = 30 * 60;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FailureScope {
    Default,
    TerminalOsLogin,
}

pub(crate) struct TerminalPolicyDecision {
    pub allowed: bool,
    pub login_error: Option<String>,
    pub audit: Option<AlarmAuditType>,
}

#[derive(Copy, Clone, Debug, Default)]
struct TerminalFailureState {
    total_failures: i32,
    backoff_until_ms: Option<i64>,
    last_failure_ms: Option<i64>,
}

lazy_static::lazy_static! {
    static ref TERMINAL_FAILURE_STATE: Mutex<TerminalFailureState> = Mutex::new(TerminalFailureState::default());
    static ref TERMINAL_OS_LOGIN_MUTEX: Arc<TokioMutex<()>> = Arc::new(TokioMutex::new(()));
}

fn terminal_os_login_backoff_seconds(total_failures: i32) -> i64 {
    if total_failures <= 2 {
        return 0;
    }
    let exp = (total_failures - 3).min(6);
    let seconds = TERMINAL_OS_LOGIN_BACKOFF_BASE_SECONDS * (1_i64 << exp);
    seconds.min(TERMINAL_OS_LOGIN_BACKOFF_MAX_SECONDS)
}

fn normalize_backoff(state: &mut TerminalFailureState, now_ms: i64) {
    if let Some(until_ms) = state.backoff_until_ms {
        if until_ms <= now_ms {
            state.backoff_until_ms = None;
        }
    }
}

fn reset_totals_on_idle(state: &mut TerminalFailureState, now_ms: i64) {
    if let Some(last_ms) = state.last_failure_ms {
        if now_ms.saturating_sub(last_ms) >= TERMINAL_OS_LOGIN_TOTAL_IDLE_RESET_MS {
            state.total_failures = 0;
            state.backoff_until_ms = None;
            state.last_failure_ms = None;
        }
    }
}

fn allow_decision() -> TerminalPolicyDecision {
    TerminalPolicyDecision {
        allowed: true,
        login_error: None,
        audit: None,
    }
}

fn block_decision(login_error: String, alarm_type: AlarmAuditType) -> TerminalPolicyDecision {
    TerminalPolicyDecision {
        allowed: false,
        login_error: Some(login_error),
        audit: Some(alarm_type),
    }
}

pub(crate) fn evaluate_terminal_policy(now_ms: i64) -> TerminalPolicyDecision {
    let mut state = TERMINAL_FAILURE_STATE.lock().unwrap();
    reset_totals_on_idle(&mut state, now_ms);
    normalize_backoff(&mut state, now_ms);

    let decision = if let Some(until_ms) = state.backoff_until_ms {
        let remaining_ms = (until_ms - now_ms).max(0);
        let remaining_seconds = ((remaining_ms + 999) / 1_000).max(1);
        log::warn!(
            "Terminal OS login blocked by backoff policy: remaining_seconds={}",
            remaining_seconds
        );
        block_decision(
            format!("Please try {} seconds later", remaining_seconds),
            AlarmAuditType::TerminalOsLoginBackoff,
        )
    } else {
        allow_decision()
    };

    decision
}

pub(crate) fn record_terminal_failure() {
    let now_ms = get_time();
    let mut state = TERMINAL_FAILURE_STATE.lock().unwrap();
    reset_totals_on_idle(&mut state, now_ms);
    normalize_backoff(&mut state, now_ms);
    state.total_failures += 1;
    state.last_failure_ms = Some(now_ms);
    let backoff_seconds = terminal_os_login_backoff_seconds(state.total_failures);
    if backoff_seconds > 0 {
        state.backoff_until_ms = Some(now_ms + backoff_seconds * 1_000);
    }
}

pub(crate) fn clear_terminal_failure_state() {
    *TERMINAL_FAILURE_STATE.lock().unwrap() = TerminalFailureState::default();
}

pub(crate) fn try_acquire_terminal_os_login_gate() -> Result<OwnedMutexGuard<()>, ()> {
    TERMINAL_OS_LOGIN_MUTEX
        .clone()
        .try_lock_owned()
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn terminal_policy_prioritizes_backoff() {
        let _guard = TEST_MUTEX.lock().unwrap();
        clear_terminal_failure_state();
        let now_ms = get_time();
        for _ in 0..3 {
            record_terminal_failure();
        }
        let decision = evaluate_terminal_policy(now_ms);
        assert!(!decision.allowed);
        assert!(decision.login_error.is_some());
        clear_terminal_failure_state();
    }

    #[test]
    fn terminal_policy_idle_window_resets_total_counter() {
        let _guard = TEST_MUTEX.lock().unwrap();
        clear_terminal_failure_state();
        let now_ms = get_time();
        for _ in 0..13 {
            record_terminal_failure();
        }
        let blocked = evaluate_terminal_policy(now_ms);
        assert!(!blocked.allowed);

        let after_idle_ms = now_ms + TERMINAL_OS_LOGIN_TOTAL_IDLE_RESET_MS + 1_000;
        let allowed = evaluate_terminal_policy(after_idle_ms);
        assert!(allowed.allowed);
    }

    #[test]
    fn terminal_policy_first_backoff_is_not_too_short() {
        let _guard = TEST_MUTEX.lock().unwrap();
        clear_terminal_failure_state();
        for _ in 0..3 {
            record_terminal_failure();
        }

        let decision = evaluate_terminal_policy(get_time());
        assert!(!decision.allowed);
        let msg = decision.login_error.unwrap_or_default();
        let seconds = msg
            .split_whitespace()
            .nth(2)
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        assert!(seconds >= TERMINAL_OS_LOGIN_BACKOFF_BASE_SECONDS);
    }
}
