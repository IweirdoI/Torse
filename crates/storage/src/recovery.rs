//! Streaming validation of a checkpoint's contiguous journal suffix.

use std::io::{self, Read};

use torse_codec::record::{self, RecordKind, RecordRead};
use torse_codec::{CodecLimits, StorageCodec};
use torse_types::{
    AuthorityData, Binding, CommitCursor, Fault, FaultCode, ModelCommit, OutputBudget,
};

use crate::journal::{io_fault, validate_record};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryReport {
    /// Last complete, validated position, or the supplied baseline for an empty journal.
    pub through: CommitCursor,
    /// Byte offset immediately after the last complete record, relative to the input start.
    pub valid_bytes: u64,
    /// Complete records delivered to the recovery builder during this scan.
    pub records: u64,
    /// Only an incomplete record at physical EOF is eligible for truncation on reopening.
    pub incomplete_tail: bool,
}

/// Scan without retaining the entire file or an unbounded list of decoded commits.
///
/// Each frame is fully checked before domain decoding. The frame limit bounds transient
/// encoded storage; `budget` separately bounds each decoded record's owned allocations.
/// The callback must feed an isolated recovery builder, not a running world. If a later record
/// fails, the caller discards that entire builder. No gameplay handlers or notifications run.
/// The builder supplies its own aggregate world-size limit; this scanner bounds one record.
/// An I/O error is never interpreted as EOF, and scanning never resynchronizes past damage.
pub fn scan_journal<M, C, R, F>(
    reader: &mut R,
    binding: Binding,
    baseline: CommitCursor,
    codec: &C,
    limits: CodecLimits,
    budget: OutputBudget,
    mut replay: F,
) -> Result<RecoveryReport, Fault>
where
    M: AuthorityData,
    C: StorageCodec<M>,
    R: Read,
    F: FnMut(ModelCommit<M>) -> Result<(), Fault>,
{
    if baseline.world != binding.world {
        return Err(Fault::invalid_data(
            "recovery baseline belongs to another world",
        ));
    }
    let mut report = RecoveryReport {
        through: baseline,
        valid_bytes: 0,
        records: 0,
        incomplete_tail: false,
    };
    loop {
        let offset = report.valid_bytes;
        let result = read_next::<M, _, _>(reader, codec, limits, budget)
            .map_err(|error| at_offset(error, offset))?;
        let (value, encoded_bytes) = match result {
            Next::End => return Ok(report),
            Next::Incomplete => {
                report.incomplete_tail = true;
                return Ok(report);
            }
            Next::Commit(value, encoded_bytes) => (value, encoded_bytes),
        };
        validate_record::<M>(binding, report.through.seq, &value)
            .map_err(|error| at_offset(error, offset))?;
        let through = value.meta.cursor;
        replay(value).map_err(|error| at_offset(error, offset))?;
        report.through = through;
        report.valid_bytes = report
            .valid_bytes
            .checked_add(encoded_bytes as u64)
            .ok_or_else(|| Fault::resource_limit("journal byte offset overflow"))?;
        report.records = report
            .records
            .checked_add(1)
            .ok_or_else(|| Fault::resource_limit("journal record count overflow"))?;
    }
}

enum Next<M: AuthorityData> {
    End,
    Incomplete,
    Commit(ModelCommit<M>, usize),
}

fn read_next<M: AuthorityData, C: StorageCodec<M>, R: Read>(
    reader: &mut R,
    codec: &C,
    limits: CodecLimits,
    budget: OutputBudget,
) -> Result<Next<M>, Fault> {
    let mut header_bytes = [0; record::HEADER_BYTES];
    let read = read_until_eof(reader, &mut header_bytes)?;
    if read == 0 {
        return Ok(Next::End);
    }
    let Some(header) = record::read_header(&header_bytes[..read], limits)? else {
        return Ok(Next::Incomplete);
    };
    if header.kind != RecordKind::Commit {
        return Err(Fault::new(
            FaultCode::CorruptData,
            "non-commit record in journal",
        ));
    }
    let total = header.encoded_bytes()?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(total)
        .map_err(|error| Fault::resource_limit(format!("reserve recovery frame: {error}")))?;
    bytes.resize(total, 0);
    bytes[..record::HEADER_BYTES].copy_from_slice(&header_bytes);
    let body = &mut bytes[record::HEADER_BYTES..];
    if read_until_eof(reader, body)? != body.len() {
        return Ok(Next::Incomplete);
    }
    match record::read_record(&bytes, limits)? {
        RecordRead::Complete { payload, .. } => {
            let value = codec.decode_commit(payload, budget)?;
            Ok(Next::Commit(value, total))
        }
        RecordRead::Incomplete => unreachable!("the validated header and body were fully read"),
    }
}

/// Like read_exact, but preserves EOF length and retries Interrupted reads. Other errors
/// propagate even when some bytes were read; transient device failure cannot authorize repair.
fn read_until_eof(reader: &mut impl Read, output: &mut [u8]) -> Result<usize, Fault> {
    let mut read = 0;
    while read < output.len() {
        match reader.read(&mut output[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_fault("read journal", error)),
        }
    }
    Ok(read)
}

fn at_offset(error: Fault, offset: u64) -> Fault {
    Fault::new(
        error.code,
        format!("journal offset {offset}: {}", error.context),
    )
}
