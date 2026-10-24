//! Transport sequencing; payload matching and durable dedup remain in authority.
use std::collections::BTreeMap;
use torse_types::*;
struct Session {
    high: u64,
    tick: Tick,
    received: u32,
}
pub struct SessionRegistry {
    next: u128,
    max: usize,
    per_tick: u32,
    sessions: BTreeMap<u128, Session>,
}
impl SessionRegistry {
    pub fn new(max: usize, per_tick: u32) -> Result<Self, Fault> {
        if max == 0 || per_tick == 0 {
            return Err(Fault::invalid_data("session limits"));
        }
        Ok(Self {
            next: 1,
            max,
            per_tick,
            sessions: BTreeMap::new(),
        })
    }
    pub fn open(&mut self) -> Result<Viewer, Fault> {
        if self.sessions.len() >= self.max || self.next == u128::MAX {
            return Err(Fault::resource_limit("session capacity"));
        }
        let id = self.next;
        self.next += 1;
        self.sessions.insert(
            id,
            Session {
                high: 0,
                tick: Tick(0),
                received: 0,
            },
        );
        Ok(Viewer {
            session: SessionId(id),
            actor: ActorContextId(id),
        })
    }
    pub fn check(&mut self, id: SessionId, seq: RequestSeq, tick: Tick) -> Result<(), RejectCode> {
        let session = self
            .sessions
            .get_mut(&id.0)
            .ok_or(RejectCode::StaleSession)?;
        if session.tick != tick {
            session.tick = tick;
            session.received = 0;
        }
        if session.received >= self.per_tick {
            return Err(RejectCode::QuotaExceeded);
        }
        session.received += 1;
        if seq.0 == 0 || seq.0 > session.high.saturating_add(1) {
            return Err(RejectCode::InvalidData);
        }
        Ok(())
    }
    pub fn admitted(&mut self, id: SessionId, seq: RequestSeq) {
        if let Some(s) = self.sessions.get_mut(&id.0) {
            s.high = s.high.max(seq.0);
        }
    }
    pub fn close(&mut self, id: SessionId) {
        self.sessions.remove(&id.0);
    }
}
