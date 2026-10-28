//! Per-frame generation, mesh, and upload limits.
//!
//! The executor's task slots and byte ceilings stay fixed. This pace only
//! chooses how much of that room one presentation frame may use. Present
//! time is reported beside these samples and does not feed the pace: a
//! fifo wait would otherwise look like the device is overloaded.

/// Most generation or mesh tasks one frame admits. Matches the default
/// executor task slots.
pub(crate) const TASK_CAP: usize = 8;
/// Most geometry bytes one frame uploads. Matches the executor input ceiling.
pub(crate) const UPLOAD_BYTE_CAP: usize = 8 * 1024 * 1024;
/// Bytes added to the upload budget when a frame was comfortably fast.
pub(crate) const UPLOAD_STEP: usize = 256 * 1024;
/// Main-thread generation and mesh time the next frame should stay under.
pub(crate) const CPU_TARGET_US: u64 = 8_000;
/// Upload time the next frame should stay under.
pub(crate) const UPLOAD_TARGET_US: u64 = 4_000;
const START_SECTIONS: usize = 4;

/// How many sections and how many upload bytes the next frame may spend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Pace {
    pub generate: usize,
    pub mesh: usize,
    pub upload_bytes: usize,
}

impl Pace {
    /// Start at the previous fixed section budget and the upload ceiling.
    /// A slow frame shrinks from here; a fast frame grows until the caps.
    pub(crate) fn new() -> Self {
        Self {
            generate: START_SECTIONS,
            mesh: START_SECTIONS,
            upload_bytes: UPLOAD_BYTE_CAP,
        }
    }

    /// Shrink after an over-budget sample and grow when the sample is under
    /// half the target. A sample inside that band leaves the budget alone.
    pub(crate) fn observe(&mut self, cpu_us: u64, upload_us: u64) {
        self.generate = scale_count(self.generate, cpu_us, CPU_TARGET_US);
        self.mesh = scale_count(self.mesh, cpu_us, CPU_TARGET_US);
        self.upload_bytes = scale_bytes(self.upload_bytes, upload_us);
    }
}

/// Workers for the shared CPU pool. One core stays with the main thread.
/// The executor rejects zero workers and more than 64.
pub(crate) fn worker_count(parallelism: usize) -> usize {
    parallelism.saturating_sub(1).clamp(1, 64)
}

/// How many leading `sizes` fit in this frame.
///
/// The first item is taken whenever the cap allows one, even when it is
/// larger than `budget_bytes`, so a shrunk budget still uploads something.
/// Later items are taken only while the running total fits and the count
/// stays within `task_cap`.
pub(crate) fn split_uploads(sizes: &[usize], budget_bytes: usize, task_cap: usize) -> usize {
    if sizes.is_empty() || task_cap == 0 {
        return 0;
    }
    let mut count = 1;
    let mut total = sizes[0];
    while count < sizes.len() && count < task_cap {
        let next = total.saturating_add(sizes[count]);
        if next > budget_bytes {
            break;
        }
        total = next;
        count += 1;
    }
    count
}

fn scale_count(value: usize, sample_us: u64, target_us: u64) -> usize {
    if sample_us > target_us {
        (value / 2).max(1)
    } else if sample_us.saturating_mul(2) < target_us {
        value.saturating_add(1).min(TASK_CAP)
    } else {
        value.clamp(1, TASK_CAP)
    }
}

fn scale_bytes(value: usize, sample_us: u64) -> usize {
    if sample_us > UPLOAD_TARGET_US {
        (value / 2).max(1)
    } else if sample_us.saturating_mul(2) < UPLOAD_TARGET_US {
        value.saturating_add(UPLOAD_STEP).min(UPLOAD_BYTE_CAP)
    } else {
        value.clamp(1, UPLOAD_BYTE_CAP)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_count_leaves_the_main_thread_and_keeps_the_executor_range() {
        assert_eq!(worker_count(0), 1);
        assert_eq!(worker_count(1), 1);
        assert_eq!(worker_count(2), 1);
        assert_eq!(worker_count(8), 7);
        assert_eq!(worker_count(65), 64);
        assert_eq!(worker_count(128), 64);
    }

    #[test]
    fn pace_shrinks_on_a_spike_and_stops_at_the_hard_caps() {
        let mut pace = Pace::new();
        for _ in 0..20 {
            pace.observe(0, 0);
        }
        assert_eq!(pace.generate, TASK_CAP);
        assert_eq!(pace.mesh, TASK_CAP);
        assert_eq!(pace.upload_bytes, UPLOAD_BYTE_CAP);

        pace.observe(50_000, 20_000);
        assert_eq!(pace.generate, TASK_CAP / 2);
        assert_eq!(pace.mesh, TASK_CAP / 2);
        assert_eq!(pace.upload_bytes, UPLOAD_BYTE_CAP / 2);

        let held = pace;
        pace.observe(CPU_TARGET_US, UPLOAD_TARGET_US);
        assert_eq!(pace, held);
        pace.observe(CPU_TARGET_US / 2, UPLOAD_TARGET_US / 2);
        assert_eq!(pace, held);

        for _ in 0..32 {
            pace.observe(u64::MAX, u64::MAX);
        }
        assert_eq!(pace.generate, 1);
        assert_eq!(pace.mesh, 1);
        assert_eq!(pace.upload_bytes, 1);

        pace.observe(0, 0);
        assert_eq!(pace.generate, 2);
        assert_eq!(pace.mesh, 2);
        assert_eq!(pace.upload_bytes, 1 + UPLOAD_STEP);
    }

    #[test]
    fn partial_upload_keeps_the_original_tail() {
        let mut pending = vec!["near", "huge", "far"];
        let sizes = [10, 50, 40];
        let count = split_uploads(&sizes, 30, TASK_CAP);
        let tail = pending.split_off(count);
        assert_eq!(pending, vec!["near"]);
        assert_eq!(tail, vec!["huge", "far"]);
    }

    #[test]
    fn upload_prefix_has_a_hard_count_and_still_moves_one_oversized_mesh() {
        assert_eq!(split_uploads(&[], 100, TASK_CAP), 0);
        assert_eq!(split_uploads(&[1, 1], 100, 0), 0);
        assert_eq!(split_uploads(&[10, 10, 10], 25, TASK_CAP), 2);
        assert_eq!(split_uploads(&[10_000_000, 1], 0, TASK_CAP), 1);
        assert_eq!(
            split_uploads(&vec![1; 30], UPLOAD_BYTE_CAP, TASK_CAP),
            TASK_CAP
        );
    }
}
