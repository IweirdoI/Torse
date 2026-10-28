//! Deterministic presentation-work feedback, independent of the logic clock.
use std::collections::BTreeSet;
use torse_types::SectionPos;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLimits {
    pub generation: usize,
    pub meshing: usize,
    pub uploads: usize,
    pub upload_bytes: usize,
}

/// Drops admission immediately on slow frames and recovers after sustained headroom.
/// Counts never exceed host hard limits; one job remains admissible for progress.
pub struct FrameBudget {
    limits: FrameLimits,
    target_us: u64,
    quarters: usize,
    good_frames: u8,
}
impl FrameBudget {
    pub fn new(limits: FrameLimits, target_us: u64) -> Self {
        Self {
            limits,
            target_us: target_us.max(1),
            quarters: 4,
            good_frames: 0,
        }
    }
    pub fn limits(&self) -> FrameLimits {
        let scale = |hard: usize| {
            if hard == 0 {
                0
            } else {
                (hard / 4 * self.quarters + hard % 4 * self.quarters / 4).max(1)
            }
        };
        FrameLimits {
            generation: scale(self.limits.generation),
            meshing: scale(self.limits.meshing),
            uploads: scale(self.limits.uploads),
            // A byte ceiling must not shrink below the size of one valid section.
            upload_bytes: self.limits.upload_bytes,
        }
    }
    pub fn observe(&mut self, elapsed_us: u64) {
        if elapsed_us > self.target_us {
            self.quarters = if elapsed_us > self.target_us.saturating_mul(2) {
                1
            } else {
                self.quarters.saturating_sub(1).max(1)
            };
            self.good_frames = 0;
        } else if elapsed_us < self.target_us.saturating_mul(3) / 4 {
            self.good_frames += 1;
            if self.good_frames >= 8 {
                self.quarters = (self.quarters + 1).min(4);
                self.good_frames = 0;
            }
        } else {
            self.good_frames = 0;
        }
    }
}

/// Coalesces edits until upload. Selection uses current sizes before allocating geometry.
#[derive(Default)]
pub struct SectionUploadQueue {
    pending: BTreeSet<SectionPos>,
}
impl SectionUploadQueue {
    pub fn extend(&mut self, sections: impl IntoIterator<Item = SectionPos>) {
        self.pending.extend(sections);
    }
    pub fn retain(&mut self, mut resident: impl FnMut(SectionPos) -> bool) {
        self.pending.retain(|p| resident(*p));
    }
    pub fn clear(&mut self) {
        self.pending.clear();
    }
    pub fn select(
        &mut self,
        center: SectionPos,
        limits: FrameLimits,
        size: impl FnMut(SectionPos) -> Option<usize>,
    ) -> Result<Vec<SectionPos>, &'static str> {
        self.select_where(center, limits, |_| true, size)
    }

    /// Defer ineligible sections without losing their pending edits.
    pub fn select_where(
        &mut self,
        center: SectionPos,
        limits: FrameLimits,
        mut eligible: impl FnMut(SectionPos) -> bool,
        mut size: impl FnMut(SectionPos) -> Option<usize>,
    ) -> Result<Vec<SectionPos>, &'static str> {
        let mut pending: Vec<_> = self.pending.iter().copied().collect();
        pending.sort_by_key(|p| {
            (
                p.x.abs_diff(center.x)
                    .saturating_add(p.y.abs_diff(center.y))
                    .saturating_add(p.z.abs_diff(center.z)),
                *p,
            )
        });
        let mut selected = Vec::new();
        let mut used = 0;
        for pos in pending {
            if selected.len() >= limits.uploads {
                break;
            }
            if !eligible(pos) {
                continue;
            }
            let Some(bytes) = size(pos) else {
                self.pending.remove(&pos);
                continue;
            };
            if bytes > limits.upload_bytes {
                return Err("section exceeds the hard upload byte limit");
            }
            if bytes > limits.upload_bytes - used {
                continue;
            }
            used += bytes;
            selected.push(pos);
        }
        for pos in &selected {
            self.pending.remove(pos);
        }
        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deferred_visibility_preserves_edits_without_expanding_geometry() {
        let mut queue = SectionUploadQueue::default();
        let section = SectionPos::ZERO;
        queue.extend([section]);
        let limits = FrameLimits {
            generation: 1,
            meshing: 1,
            uploads: 1,
            upload_bytes: 8,
        };
        assert!(
            queue
                .select_where(
                    section,
                    limits,
                    |_| false,
                    |_| panic!("deferred geometry must not be expanded")
                )
                .unwrap()
                .is_empty()
        );
        assert!(queue.pending.contains(&section));
        assert_eq!(
            queue
                .select_where(section, limits, |_| true, |_| Some(8))
                .unwrap(),
            [section]
        );
        assert!(queue.pending.is_empty());
    }
    #[test]
    fn sudden_load_backs_off_then_recovers_without_changing_hard_limits() {
        let limits = FrameLimits {
            generation: 4,
            meshing: 8,
            uploads: 4,
            upload_bytes: 8 << 20,
        };
        let mut budget = FrameBudget::new(limits, 16_666);
        budget.observe(100_000);
        assert_eq!(budget.limits().generation, 1);
        assert_eq!(budget.limits().meshing, 2);
        assert_eq!(budget.limits().uploads, 1);
        assert_eq!(budget.limits().upload_bytes, limits.upload_bytes);
        for _ in 0..24 {
            budget.observe(1000);
        }
        assert_eq!(budget.limits(), limits);
        for sample in [u64::MAX, 0, 16_666, 1, 50_000, 500] {
            budget.observe(sample);
            let now = budget.limits();
            assert!(
                now.generation <= limits.generation
                    && now.meshing <= limits.meshing
                    && now.uploads <= limits.uploads
            );
        }
        let zero = FrameLimits {
            generation: 0,
            meshing: 0,
            uploads: 0,
            upload_bytes: 0,
        };
        assert_eq!(FrameBudget::new(zero, 0).limits(), zero);
    }
    #[test]
    fn uploads_coalesce_defer_by_bytes_and_drop_unloaded_sections() {
        let mut queue = SectionUploadQueue::default();
        let a = SectionPos::ZERO;
        let b = SectionPos::new(1, 0, 0);
        let c = SectionPos::new(2, 0, 0);
        queue.extend([a, a, b, c]);
        let limits = FrameLimits {
            generation: 1,
            meshing: 1,
            uploads: 2,
            upload_bytes: 10,
        };
        assert_eq!(queue.select(a, limits, |_| Some(6)).unwrap(), [a]);
        queue.retain(|p| p != b);
        assert_eq!(queue.select(a, limits, |_| Some(6)).unwrap(), [c]);
        assert!(queue.pending.is_empty());
        queue.extend([a]);
        assert!(queue.select(a, limits, |_| Some(11)).is_err());
        assert!(queue.pending.contains(&a));
        assert!(
            queue
                .select(
                    a,
                    FrameLimits {
                        uploads: 0,
                        ..limits
                    },
                    |_| Some(1)
                )
                .unwrap()
                .is_empty()
        );
    }
}
