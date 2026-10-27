//! Versioned retention roots and bounded journal segments under the journal's stable lock.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use torse_codec::StorageCodec;
use torse_types::{AuthorityData, Binding, CommitCursor, CommitSeq, Fault, ModelCommit};

use crate::journal::io_fault;
use crate::manifest::corrupt;
use crate::{JournalConfig, RecoveryReport, scan_journal};

const MAGIC: &[u8; 8] = b"TORSEJS1";
const HEAD_BYTES: usize = 64;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Anchor {
    pub base: CommitCursor,
    pub legacy_end: CommitSeq,
    pub floor: CommitSeq,
    pub legacy_bytes: u64,
}

pub(crate) fn root(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".segments");
    PathBuf::from(name)
}

pub(crate) fn regular(path: &Path, missing: bool) -> Result<(), Fault> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(()),
        Err(e) if missing && e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_fault("inspect journal file", e)),
        Ok(_) => Err(corrupt("journal path must be a regular file")),
    }
}

fn directory(path: &Path) -> Result<bool, Fault> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_fault("inspect segment directory", e)),
        Ok(_) => Err(corrupt("segment root must be a real directory")),
    }
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), Fault> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| io_fault("sync journal directory", e))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn read_anchor(root: &Path, binding: Binding) -> Result<Option<Anchor>, Fault> {
    if !directory(root)? {
        return Ok(None);
    }
    let path = root.join("head");
    regular(&path, true)?;
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_fault("open journal retention root", e)),
    };
    if file
        .metadata()
        .map_err(|e| io_fault("stat retention root", e))?
        .len()
        != HEAD_BYTES as u64
    {
        return Err(corrupt("invalid journal retention root length"));
    }
    let mut bytes = [0; HEAD_BYTES];
    file.read_exact(&mut bytes)
        .map_err(|e| io_fault("read retention root", e))?;
    if &bytes[..8] != MAGIC
        || &bytes[60..] != b"JEND"
        || crc32c::crc32c(&bytes[..56]) != u32::from_le_bytes(bytes[56..60].try_into().unwrap())
    {
        return Err(corrupt(
            "invalid journal retention root version or checksum",
        ));
    }
    let world = u128::from_le_bytes(bytes[8..24].try_into().unwrap());
    let base = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
    let legacy_end = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
    let floor = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
    if world != binding.world.0 || base > legacy_end || floor < base {
        return Err(corrupt(
            "invalid journal retention root identity or positions",
        ));
    }
    Ok(Some(Anchor {
        base: CommitCursor {
            world: binding.world,
            seq: CommitSeq(base),
        },
        legacy_end: CommitSeq(legacy_end),
        floor: CommitSeq(floor),
        legacy_bytes: u64::from_le_bytes(bytes[48..56].try_into().unwrap()),
    }))
}

pub(crate) fn publish_anchor(root: &Path, anchor: Anchor) -> Result<(), Fault> {
    let mut bytes = [0; HEAD_BYTES];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..24].copy_from_slice(&anchor.base.world.0.to_le_bytes());
    bytes[24..32].copy_from_slice(&anchor.base.seq.0.to_le_bytes());
    bytes[32..40].copy_from_slice(&anchor.legacy_end.0.to_le_bytes());
    bytes[40..48].copy_from_slice(&anchor.floor.0.to_le_bytes());
    bytes[48..56].copy_from_slice(&anchor.legacy_bytes.to_le_bytes());
    let crc = crc32c::crc32c(&bytes[..56]);
    bytes[56..60].copy_from_slice(&crc.to_le_bytes());
    bytes[60..].copy_from_slice(b"JEND");
    let temporary = root.join("head.next");
    regular(&temporary, true)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|e| io_fault("create retention root", e))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| io_fault("persist retention root", e))?;
    drop(file);
    fs::rename(&temporary, root.join("head")).map_err(|e| io_fault("publish retention root", e))?;
    sync_directory(root)
}

#[derive(Debug)]
pub(crate) struct Segment {
    pub first: u64,
    pub path: PathBuf,
    pub length: u64,
}

/// Keep a recognizable marker in the stable lock file so legacy readers fail closed.
/// The already-durable root makes every overwritten byte unreachable first.
pub(crate) fn install_marker(file: &mut File, anchor: Anchor) -> Result<(), Fault> {
    let offset = if anchor.floor >= anchor.legacy_end {
        0
    } else {
        anchor.legacy_bytes
    };
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.write_all(MAGIC))
        .and_then(|_| file.set_len(offset + MAGIC.len() as u64))
        .and_then(|_| file.sync_all())
        .map_err(|e| io_fault("persist segmented journal marker", e))
}

pub(crate) fn list(root: &Path, config: JournalConfig) -> Result<Vec<Segment>, Fault> {
    let mut files = Vec::new();
    if !directory(root)? {
        return Ok(files);
    }
    for (index, entry) in fs::read_dir(root)
        .map_err(|e| io_fault("list journal segments", e))?
        .enumerate()
    {
        if index as u64 >= u64::from(config.max_segment_files) + 2 {
            return Err(Fault::resource_limit(
                "journal segment directory capacity exceeded",
            ));
        }
        let entry = entry.map_err(|e| io_fault("read segment entry", e))?;
        regular(&entry.path(), false)?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| corrupt("invalid segment filename"))?;
        if matches!(name, "head" | "head.next") {
            continue;
        }
        let first = name
            .strip_suffix(".seg")
            .filter(|s| {
                s.len() == 16
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .filter(|n| *n != 0)
            .ok_or_else(|| corrupt("unexpected journal segment filename"))?;
        let length = entry
            .metadata()
            .map_err(|e| io_fault("stat segment", e))?
            .len();
        if length > config.max_segment_bytes || files.len() >= config.max_segment_files as usize {
            return Err(Fault::resource_limit("journal segment capacity exceeded"));
        }
        files
            .try_reserve(1)
            .map_err(|_| Fault::resource_limit("segment list allocation"))?;
        files.push(Segment {
            first,
            path: entry.path(),
            length,
        });
    }
    files.sort_unstable_by_key(|s| s.first);
    Ok(files)
}

pub(crate) struct Inspection {
    pub report: RecoveryReport,
    pub anchor: Option<Anchor>,
    pub files: Vec<Segment>,
    pub last_valid: u64,
    pub last_through: CommitSeq,
}

/// Validate disk storage without creating files, truncating tails, or synchronizing.
/// A segmented retention root must not be newer than the supplied trusted checkpoint.
pub fn scan_file_journal<M: AuthorityData, C: StorageCodec<M>>(
    path: impl AsRef<Path>,
    binding: Binding,
    baseline: CommitCursor,
    checkpoint: CommitCursor,
    codec: &C,
    config: JournalConfig,
    replay: impl FnMut(ModelCommit<M>) -> Result<(), Fault>,
) -> Result<RecoveryReport, Fault> {
    let path = path.as_ref();
    let mut file = File::open(path).map_err(|e| io_fault("open journal scan", e))?;
    Ok(inspect(
        (path, &mut file),
        binding,
        baseline,
        checkpoint,
        codec,
        config,
        replay,
    )?
    .report)
}

pub(crate) fn inspect<M: AuthorityData, C: StorageCodec<M>>(
    source: (&Path, &mut File),
    binding: Binding,
    baseline: CommitCursor,
    checkpoint: CommitCursor,
    codec: &C,
    config: JournalConfig,
    mut replay: impl FnMut(ModelCommit<M>) -> Result<(), Fault>,
) -> Result<Inspection, Fault> {
    let (path, legacy) = source;
    config.validate()?;
    regular(path, false)?;
    let root = root(path);
    let anchor = read_anchor(&root, binding)?;
    let files = list(&root, config)?;
    let Some(anchor) = anchor else {
        if !files.is_empty() {
            return Err(corrupt("segments have no durable retention root"));
        }
        legacy
            .seek(SeekFrom::Start(0))
            .map_err(|e| io_fault("seek journal scan", e))?;
        let report = scan_journal(
            legacy,
            binding,
            baseline,
            codec,
            config.codec_limits,
            config.codec_budget,
            replay,
        )?;
        return Ok(Inspection {
            report,
            anchor: None,
            files,
            last_valid: report.valid_bytes,
            last_through: report.through.seq,
        });
    };
    if checkpoint.world != binding.world || anchor.floor > checkpoint.seq {
        return Err(Fault::new(
            torse_types::FaultCode::SequenceGap,
            "journal retention floor exceeds trusted checkpoint",
        ));
    }
    let mut report = RecoveryReport {
        through: CommitCursor {
            world: binding.world,
            seq: anchor.floor,
        },
        valid_bytes: 0,
        records: 0,
        incomplete_tail: false,
    };
    if anchor.floor < anchor.legacy_end {
        legacy
            .seek(SeekFrom::Start(0))
            .map_err(|e| io_fault("seek retained legacy journal", e))?;
        let mut file = legacy.take(anchor.legacy_bytes);
        report = scan_journal(
            &mut file,
            binding,
            anchor.base,
            codec,
            config.codec_limits,
            config.codec_budget,
            |record| {
                if record.meta.cursor.seq > anchor.floor {
                    replay(record)?;
                }
                Ok(())
            },
        )?;
        if report.incomplete_tail
            || report.through.seq != anchor.legacy_end
            || report.valid_bytes != anchor.legacy_bytes
        {
            return Err(Fault::new(
                torse_types::FaultCode::SequenceGap,
                "legacy journal differs from retention root",
            ));
        }
    }
    let mut last_valid = 0;
    let mut last_through = anchor.floor;
    for (index, segment) in files.iter().enumerate() {
        // A root publication may survive before any or all obsolete files are deleted.
        if files
            .get(index + 1)
            .is_some_and(|next| next.first <= anchor.floor.0.saturating_add(1))
        {
            continue;
        }
        if segment.first <= anchor.legacy_end.0
            || segment.first > report.through.seq.0.saturating_add(1)
            || (index != 0
                && segment.first <= report.through.seq.0
                && segment.first > anchor.floor.0)
        {
            return Err(corrupt("journal segment sequence gap or overlap"));
        }
        let start = CommitCursor {
            world: binding.world,
            seq: CommitSeq(segment.first - 1),
        };
        let mut file =
            File::open(&segment.path).map_err(|e| io_fault("open journal segment", e))?;
        let part = scan_journal(
            &mut file,
            binding,
            start,
            codec,
            config.codec_limits,
            config.codec_budget,
            |record| {
                if record.meta.cursor.seq > anchor.floor {
                    replay(record)?;
                }
                Ok(())
            },
        )?;
        if (part.incomplete_tail || part.records == 0) && index + 1 != files.len() {
            return Err(corrupt("incomplete non-final journal segment"));
        }
        if let Some(next) = files.get(index + 1)
            && part.through.seq.0.checked_add(1) != Some(next.first)
        {
            return Err(corrupt("journal segment boundary mismatch"));
        }
        report.through.seq = report.through.seq.max(part.through.seq);
        report.valid_bytes = report
            .valid_bytes
            .checked_add(part.valid_bytes)
            .ok_or_else(|| Fault::resource_limit("journal scan byte overflow"))?;
        report.records = report
            .records
            .checked_add(part.records)
            .ok_or_else(|| Fault::resource_limit("journal scan count overflow"))?;
        report.incomplete_tail = part.incomplete_tail;
        last_valid = part.valid_bytes;
        last_through = part.through.seq;
    }
    Ok(Inspection {
        report,
        anchor: Some(anchor),
        files,
        last_valid,
        last_through,
    })
}

#[derive(Debug)]
pub(crate) struct SegmentWriter {
    pub root: PathBuf,
    pub anchor: Anchor,
    pub active: Option<(File, u64)>,
    pub files: u32,
    pub next: u64,
    pub config: JournalConfig,
}

impl SegmentWriter {
    pub fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if self
            .active
            .as_ref()
            .is_none_or(|(_, length)| *length + bytes.len() as u64 > self.config.max_segment_bytes)
        {
            self.sync()?;
            if self.files >= self.config.max_segment_files {
                return Err(std::io::Error::other(
                    "journal segment file capacity exhausted",
                ));
            }
            let path = self.root.join(format!("{:016x}.seg", self.next));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)?;
            self.active = Some((file, 0));
            self.files += 1;
        }
        let (file, length) = self.active.as_mut().expect("active segment exists");
        file.write_all(bytes)?;
        *length += bytes.len() as u64;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("journal sequence exhausted"))?;
        Ok(())
    }
    pub fn sync(&mut self) -> std::io::Result<()> {
        if let Some((file, _)) = &self.active {
            file.sync_all()?;
        }
        sync_directory(&self.root).map_err(|e| std::io::Error::other(e.context))
    }
}
