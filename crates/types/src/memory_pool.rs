//! Shared logical memory reservations with ownership-bound release.
use crate::{
    EvictionPlan, EvictionRank, Fault, MemoryBudget, MemoryChargeId, MemoryClass, MemoryLedger,
    MemoryResident,
};
use std::sync::{Arc, Mutex, MutexGuard};

struct Account {
    ledger: MemoryLedger,
    peak: [u64; MemoryClass::COUNT],
    total_peak: u64,
}
impl Account {
    fn sample(&mut self) {
        for class in MemoryClass::ALL {
            self.peak[class.index()] = self.peak[class.index()].max(self.ledger.usage(class));
        }
        self.total_peak = self.total_peak.max(self.ledger.total_usage());
    }
}
/// Clones share one process and class account. Native allocation remains trusted.
#[derive(Clone)]
pub struct MemoryPool(Arc<Mutex<Account>>);
impl core::fmt::Debug for MemoryPool {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MemoryPool")
            .field("usage", &self.usage())
            .finish()
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MemoryUsage {
    pub current: [u64; MemoryClass::COUNT],
    pub peak: [u64; MemoryClass::COUNT],
    pub total: u64,
    pub total_peak: u64,
}
impl MemoryPool {
    pub fn new(budget: MemoryBudget) -> Self {
        Self(Arc::new(Mutex::new(Account {
            ledger: MemoryLedger::new(budget),
            peak: [0; MemoryClass::COUNT],
            total_peak: 0,
        })))
    }
    fn account(&self) -> MutexGuard<'_, Account> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub fn budget(&self) -> MemoryBudget {
        self.account().ledger.budget()
    }
    pub fn usage(&self) -> MemoryUsage {
        let account = self.account();
        MemoryUsage {
            current: MemoryClass::ALL.map(|class| account.ledger.usage(class)),
            peak: account.peak,
            total: account.ledger.total_usage(),
            total_peak: account.total_peak,
        }
    }
    /// Reserve before constructing owned input or buffers. Zero creates an empty lease.
    pub fn reserve(
        &self,
        class: MemoryClass,
        bytes: u64,
        rank: EvictionRank,
    ) -> Result<MemoryLease, Fault> {
        let mut account = self.account();
        let id = if bytes == 0 {
            None
        } else {
            Some(
                account
                    .ledger
                    .charge(MemoryResident { class, bytes, rank })?,
            )
        };
        account.sample();
        Ok(MemoryLease {
            pool: self.clone(),
            id,
            class,
            bytes,
            rank,
        })
    }
    pub fn eviction_plan(&self, needed: u64) -> EvictionPlan {
        self.account().ledger.eviction_plan(needed)
    }
}

/// A reservation cannot be cloned. Move it with the owned data it accounts.
#[derive(Debug)]
#[must_use = "keep the lease for the entire accounted lifetime"]
pub struct MemoryLease {
    pool: MemoryPool,
    id: Option<MemoryChargeId>,
    class: MemoryClass,
    bytes: u64,
    rank: EvictionRank,
}
impl MemoryLease {
    pub fn pool(&self) -> &MemoryPool {
        &self.pool
    }
    pub fn id(&self) -> Option<MemoryChargeId> {
        self.id
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn resize(&mut self, bytes: u64) -> Result<(), Fault> {
        self.replace(bytes, self.class, self.rank)
    }
    pub fn replace(
        &mut self,
        bytes: u64,
        class: MemoryClass,
        rank: EvictionRank,
    ) -> Result<(), Fault> {
        let mut account = self.pool.account();
        let incoming: Vec<_> = (bytes > 0)
            .then_some(MemoryResident { class, bytes, rank })
            .into_iter()
            .collect();
        let ids = account
            .ledger
            .exchange(&self.id.into_iter().collect::<Vec<_>>(), &incoming)?;
        self.id = ids.first().copied();
        self.bytes = bytes;
        self.class = class;
        self.rank = rank;
        account.sample();
        Ok(())
    }
    /// Transfer bytes without temporarily charging both producer and consumer.
    /// Failure preserves the complete producer reservation.
    pub fn split(
        &mut self,
        class: MemoryClass,
        bytes: u64,
        rank: EvictionRank,
    ) -> Result<Self, Fault> {
        if bytes > self.bytes {
            return Err(Fault::invalid_data("memory transfer exceeds reservation"));
        }
        let remaining = self.bytes - bytes;
        let mut account = self.pool.account();
        let mut incoming = Vec::new();
        if remaining > 0 {
            incoming.push(MemoryResident {
                class: self.class,
                bytes: remaining,
                rank: self.rank,
            });
        }
        if bytes > 0 {
            incoming.push(MemoryResident { class, bytes, rank });
        }
        let ids = account
            .ledger
            .exchange(&self.id.into_iter().collect::<Vec<_>>(), &incoming)?;
        self.id = if remaining > 0 {
            ids.first().copied()
        } else {
            None
        };
        self.bytes = remaining;
        let id = if bytes > 0 { ids.last().copied() } else { None };
        account.sample();
        Ok(Self {
            pool: self.pool.clone(),
            id,
            class,
            bytes,
            rank,
        })
    }
    /// Merge a prepared growth reservation into a published resident atomically.
    pub fn absorb(&mut self, other: &mut Self, bytes: u64) -> Result<(), Fault> {
        if !Arc::ptr_eq(&self.pool.0, &other.pool.0) || self.class != other.class {
            return Err(Fault::invalid_data(
                "memory reservations belong to different accounts or classes",
            ));
        }
        let mut account = self.pool.account();
        let release: Vec<_> = self.id.into_iter().chain(other.id).collect();
        let incoming: Vec<_> = (bytes > 0)
            .then_some(MemoryResident {
                class: self.class,
                bytes,
                rank: self.rank,
            })
            .into_iter()
            .collect();
        let ids = account.ledger.exchange(&release, &incoming)?;
        self.id = ids.first().copied();
        self.bytes = bytes;
        other.id = None;
        other.bytes = 0;
        account.sample();
        Ok(())
    }
}
impl Drop for MemoryLease {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let _ = self.pool.account().ledger.release(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_reservations_transfer_atomically_and_release_after_failure() {
        let pool = MemoryPool::new(MemoryBudget::new(100).with_class_limit(MemoryClass::Mesh, 30));
        let mut job = pool
            .reserve(MemoryClass::Executor, 100, EvictionRank::Pinned)
            .unwrap();
        assert!(
            job.split(MemoryClass::Mesh, 31, EvictionRank::Mesh { distance_sq: 2 })
                .is_err()
        );
        assert_eq!(pool.usage().total, 100);
        let mesh = job
            .split(MemoryClass::Mesh, 30, EvictionRank::Mesh { distance_sq: 2 })
            .unwrap();
        assert_eq!(pool.usage().current[MemoryClass::Mesh.index()], 30);
        assert!(
            pool.reserve(MemoryClass::Codec, 1, EvictionRank::Pinned)
                .is_err()
        );
        drop(job);
        let temporary = pool
            .reserve(MemoryClass::Codec, 70, EvictionRank::Pinned)
            .unwrap();
        assert_eq!(pool.eviction_plan(1).charges, [mesh.id().unwrap()]);
        drop(temporary);
        drop(mesh);
        assert_eq!(pool.usage().total, 0);
        assert_eq!(pool.usage().total_peak, 100);
    }
    #[test]
    fn concurrent_reservations_share_a_single_process_ceiling() {
        let pool = MemoryPool::new(MemoryBudget::new(80));
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let pool = pool.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                let held = pool
                    .reserve(MemoryClass::Executor, 10, EvictionRank::Pinned)
                    .unwrap();
                barrier.wait();
                barrier.wait();
                drop(held);
            }));
        }
        barrier.wait();
        assert_eq!(pool.usage().total, 80);
        assert!(
            pool.reserve(MemoryClass::Codec, 1, EvictionRank::Pinned)
                .is_err()
        );
        barrier.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(pool.usage().total, 0);
    }
}
