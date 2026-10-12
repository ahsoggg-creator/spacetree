use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed};

pub const PHASE_SCANNING: u8 = 0;
pub const PHASE_BUILDING: u8 = 1;

/// Shared between the scanning thread(s) and the observer (CLI/GUI).
#[derive(Default)]
pub struct Progress {
    cancel: AtomicBool,
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    /// Known amount of work (MFT records), 0 if unknown.
    pub total_units: AtomicU64,
    pub done_units: AtomicU64,
    pub phase: AtomicU8,
}

impl Progress {
    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }

    /// Fraction in 0..=1 when the total is known.
    pub fn fraction(&self) -> Option<f32> {
        let t = self.total_units.load(Relaxed);
        (t > 0).then(|| (self.done_units.load(Relaxed) as f32 / t as f32).min(1.0))
    }
}
