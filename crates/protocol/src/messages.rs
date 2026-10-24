use torse_codec::{Decoder, EncodeResult, Encoder, wire::Wire};
use torse_types::*;
pub const PROTOCOL_VERSION: u16 = 1;
#[derive(Debug)]
pub enum ClientMessage<I> {
    Hello {
        version: u16,
        compatibility: Compatibility,
    },
    Request {
        seq: RequestSeq,
        call: I,
    },
    Subscribe {
        sections: Vec<SectionPos>,
    },
    Unsubscribe {
        subscription: SubscriptionId,
    },
    Close,
}
#[derive(Debug)]
pub enum ServerMessage<O, S, C> {
    Welcome {
        binding: Binding,
        session: SessionId,
    },
    Admission {
        seq: RequestSeq,
        admission: Admission,
    },
    Result {
        seq: RequestSeq,
        reply: O,
    },
    Seed(ReplicaSeed<S>),
    Batch(ReplicaBatch<C>),
    Rejected(RejectCode),
}
fn invalid() -> Fault {
    Fault::invalid_data("unknown protocol message/tag")
}
impl<I: Wire> Wire for ClientMessage<I> {
    fn write(&self, e: &mut Encoder<'_>) -> EncodeResult<()> {
        match self {
            Self::Hello {
                version,
                compatibility,
            } => {
                0u8.write(e)?;
                version.write(e)?;
                compatibility.write(e)
            }
            Self::Request { seq, call } => {
                1u8.write(e)?;
                seq.write(e)?;
                call.write(e)
            }
            Self::Subscribe { sections } => {
                2u8.write(e)?;
                sections.write(e)
            }
            Self::Unsubscribe { subscription } => {
                3u8.write(e)?;
                subscription.write(e)
            }
            Self::Close => 4u8.write(e),
        }
    }
    fn read(d: &mut Decoder<'_>) -> EncodeResult<Self> {
        match u8::read(d)? {
            0 => Ok(Self::Hello {
                version: Wire::read(d)?,
                compatibility: Wire::read(d)?,
            }),
            1 => Ok(Self::Request {
                seq: Wire::read(d)?,
                call: Wire::read(d)?,
            }),
            2 => Ok(Self::Subscribe {
                sections: Wire::read(d)?,
            }),
            3 => Ok(Self::Unsubscribe {
                subscription: Wire::read(d)?,
            }),
            4 => Ok(Self::Close),
            _ => Err(invalid()),
        }
    }
}
// Admission belongs to the authority vocabulary; use a local encoding helper
// rather than an orphan trait implementation in this protocol crate.
fn write_admission(value: &Admission, e: &mut Encoder<'_>) -> EncodeResult<()> {
    match value {
        Admission::Queued { target_tick } => {
            0u8.write(e)?;
            target_tick.write(e)
        }
        Admission::Duplicate => 1u8.write(e),
        Admission::Rejected(r) => {
            2u8.write(e)?;
            r.write(e)
        }
    }
}
fn read_admission(d: &mut Decoder<'_>) -> EncodeResult<Admission> {
    match u8::read(d)? {
        0 => Ok(Admission::Queued {
            target_tick: Wire::read(d)?,
        }),
        1 => Ok(Admission::Duplicate),
        2 => Ok(Admission::Rejected(Wire::read(d)?)),
        _ => Err(invalid()),
    }
}
impl<O: Wire, S: Wire, C: Wire> Wire for ServerMessage<O, S, C> {
    fn write(&self, e: &mut Encoder<'_>) -> EncodeResult<()> {
        match self {
            Self::Welcome { binding, session } => {
                0u8.write(e)?;
                binding.write(e)?;
                session.write(e)
            }
            Self::Admission { seq, admission } => {
                1u8.write(e)?;
                seq.write(e)?;
                write_admission(admission, e)
            }
            Self::Result { seq, reply } => {
                2u8.write(e)?;
                seq.write(e)?;
                reply.write(e)
            }
            Self::Seed(seed) => {
                3u8.write(e)?;
                seed.write(e)
            }
            Self::Batch(batch) => {
                4u8.write(e)?;
                batch.write(e)
            }
            Self::Rejected(reason) => {
                5u8.write(e)?;
                reason.write(e)
            }
        }
    }
    fn read(d: &mut Decoder<'_>) -> EncodeResult<Self> {
        match u8::read(d)? {
            0 => Ok(Self::Welcome {
                binding: Wire::read(d)?,
                session: Wire::read(d)?,
            }),
            1 => Ok(Self::Admission {
                seq: Wire::read(d)?,
                admission: read_admission(d)?,
            }),
            2 => Ok(Self::Result {
                seq: Wire::read(d)?,
                reply: Wire::read(d)?,
            }),
            3 => Ok(Self::Seed(Wire::read(d)?)),
            4 => Ok(Self::Batch(Wire::read(d)?)),
            5 => Ok(Self::Rejected(Wire::read(d)?)),
            _ => Err(invalid()),
        }
    }
}
