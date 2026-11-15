//! Locking helpers shared by the daemon's modules.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock a mutex, taking the guard even when a holder panicked: every value
/// behind these mutexes stays consistent on its own, so a poisoned lock
/// is a lost holder rather than corrupt state.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
