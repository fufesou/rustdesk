use hbb_common::{bail, ResultType};
use std::sync::atomic::{AtomicU8, Ordering};

const MODE_UNINITIALIZED: u8 = 0;
const MODE_PROTECTED_ONLY: u8 = 1;
const MODE_MIGRATION_READINESS: u8 = 2;
const MODE_LEGACY_ROLLBACK: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ServiceIpcMode {
    ProtectedOnly,
    MigrationReadiness,
    LegacyRollback,
}

impl ServiceIpcMode {
    fn value(self) -> u8 {
        match self {
            Self::ProtectedOnly => MODE_PROTECTED_ONLY,
            Self::MigrationReadiness => MODE_MIGRATION_READINESS,
            Self::LegacyRollback => MODE_LEGACY_ROLLBACK,
        }
    }

    fn from_value(value: u8) -> Option<Self> {
        match value {
            MODE_PROTECTED_ONLY => Some(Self::ProtectedOnly),
            MODE_MIGRATION_READINESS => Some(Self::MigrationReadiness),
            MODE_LEGACY_ROLLBACK => Some(Self::LegacyRollback),
            _ => None,
        }
    }

    pub(super) fn protected_ipc_enabled(self) -> bool {
        self != Self::MigrationReadiness
    }

    pub(super) fn after_migration_completion(self) -> Option<Self> {
        match self {
            Self::MigrationReadiness | Self::ProtectedOnly => Some(Self::ProtectedOnly),
            Self::LegacyRollback => None,
        }
    }
}

static SERVICE_IPC_MODE: AtomicU8 = AtomicU8::new(MODE_UNINITIALIZED);

pub(super) fn record_service_ipc_mode(mode: ServiceIpcMode) -> ResultType<()> {
    match SERVICE_IPC_MODE.compare_exchange(
        MODE_UNINITIALIZED,
        mode.value(),
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(()),
        Err(current) if current == mode.value() => Ok(()),
        Err(_) => bail!("macOS service IPC mode was already initialized differently"),
    }
}

pub(in crate::platform::macos) fn complete_migration_readiness() -> ResultType<()> {
    let current_value = SERVICE_IPC_MODE.load(Ordering::Acquire);
    let Some(current) = ServiceIpcMode::from_value(current_value) else {
        bail!("macOS service IPC mode is not initialized");
    };
    let Some(next) = current.after_migration_completion() else {
        bail!("legacy rollback IPC cannot transition to protected IPC");
    };
    match SERVICE_IPC_MODE.compare_exchange(
        current_value,
        next.value(),
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(()),
        Err(value) if value == next.value() => Ok(()),
        Err(_) => bail!("macOS service IPC mode changed during migration completion"),
    }
}

pub(crate) fn protected_service_ipc_enabled() -> bool {
    ServiceIpcMode::from_value(SERVICE_IPC_MODE.load(Ordering::Acquire))
        .is_some_and(ServiceIpcMode::protected_ipc_enabled)
}

pub(crate) fn legacy_rollback_ipc_enabled() -> bool {
    ServiceIpcMode::from_value(SERVICE_IPC_MODE.load(Ordering::Acquire))
        == Some(ServiceIpcMode::LegacyRollback)
}
