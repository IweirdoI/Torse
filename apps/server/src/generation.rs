//! A bounded, independently inspectable identity for deterministic terrain generation.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use torse_codec::{
    CodecLimits,
    wire::{self, Wire},
};
use torse_types::{ContentKey, Digest, Fault, OutputBudget};

const LIMIT: u64 = 512;
const MAGIC: &[u8; 8] = b"TORSEGN1";
const CODEC: CodecLimits = CodecLimits {
    max_frame_bytes: 512,
    max_field_bytes: 256,
    max_depth: 4,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationIdentity {
    pub id: ContentKey,
    pub version: u32,
    /// Digest every seed, algorithm parameter, and other input that affects generated data.
    pub configuration: Digest,
}

impl Wire for GenerationIdentity {
    fn write(&self, e: &mut torse_codec::Encoder<'_>) -> Result<(), Fault> {
        if self.version == 0 {
            return Err(Fault::invalid_data("generation version must be positive"));
        }
        self.id.write(e)?;
        self.version.write(e)?;
        self.configuration.write(e)
    }
    fn read(d: &mut torse_codec::Decoder<'_>) -> Result<Self, Fault> {
        let identity = Self {
            id: Wire::read(d)?,
            version: Wire::read(d)?,
            configuration: Wire::read(d)?,
        };
        if identity.version == 0 {
            return Err(Fault::invalid_data("invalid generation version"));
        }
        Ok(identity)
    }
}

impl GenerationIdentity {
    fn encode(&self) -> Result<Vec<u8>, Fault> {
        if self.version == 0 {
            return Err(Fault::invalid_data("generation version must be positive"));
        }
        let mut bytes = MAGIC.to_vec();
        wire::encode(self, &mut bytes, OutputBudget::new(32, LIMIT), CODEC)?;
        let crc = crc32c::crc32c(&bytes);
        bytes.extend_from_slice(&crc.to_le_bytes());
        Ok(bytes)
    }

    /// Inspect under a WorldLease before allocating a new epoch or activating storage.
    /// A missing descriptor is explicit and does not create files.
    pub fn inspect(root: impl AsRef<Path>) -> Result<Option<Self>, Fault> {
        let path = root.as_ref().join("generation.bin");
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_file() && meta.len() <= LIMIT => {}
            Ok(_) => return Err(Fault::invalid_data("invalid generation descriptor file")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(e)),
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(io)?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        if bytes.len() < 12 || bytes.len() as u64 > LIMIT || &bytes[..8] != MAGIC {
            return Err(Fault::invalid_data("invalid generation descriptor header"));
        }
        let end = bytes.len() - 4;
        if crc32c::crc32c(&bytes[..end]) != u32::from_le_bytes(bytes[end..].try_into().unwrap()) {
            return Err(Fault::invalid_data(
                "generation descriptor checksum mismatch",
            ));
        }
        let identity: Self = wire::decode(&bytes[8..end], OutputBudget::new(32, LIMIT), CODEC)?;
        if identity.version == 0 {
            return Err(Fault::invalid_data("invalid generation version"));
        }
        Ok(Some(identity))
    }

    pub fn verify(&self, root: impl AsRef<Path>) -> Result<bool, Fault> {
        match Self::inspect(root)? {
            Some(identity) if identity != *self => Err(Fault::incompatible(
                "generation identity, version, or configuration changed",
            )),
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }

    /// Explicitly adopt this generator while the caller holds the world's exclusive lease.
    /// Once registered, replacement requires an independent world migration.
    pub(crate) fn bind(&self, root: &Path) -> Result<(), Fault> {
        let bytes = self.encode()?;
        if self.verify(root)? {
            return Ok(());
        }
        let temporary = root.join("generation.next");
        match fs::symlink_metadata(&temporary) {
            Ok(meta) if meta.file_type().is_file() => {}
            Ok(_) => {
                return Err(Fault::invalid_data(
                    "generation draft must be a regular file",
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .map_err(io)?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(io)?;
        drop(file);
        fs::rename(temporary, root.join("generation.bin")).map_err(io)?;
        crate::directory::sync_directory(root)
    }
}

fn io(error: std::io::Error) -> Fault {
    Fault::new(
        torse_types::FaultCode::StorageUnavailable,
        format!("generation descriptor: {error}"),
    )
}
