//! Stable content and object identities shared by engine boundaries.
//!
//! Identity strings are validated at construction time and then stored as owned
//! components. This keeps malformed names out of registries, save files, and
//! network messages without forcing every consumer to repeat the same parsing
//! rules. Separate persistent and ephemeral object identifiers prevent runtime
//! slots from accidentally becoming save-file references.

use core::fmt;

const MAX_NAMESPACE_LEN: usize = 64;
const MAX_PATH_LEN: usize = 128;
const MAX_CONTENT_KEY_LEN: usize = MAX_NAMESPACE_LEN + 1 + MAX_PATH_LEN;

/// Fully qualified content identity such as `base:stone`.
///
/// The namespace identifies the package or built-in domain and the path names
/// the item inside that domain. Both components are intentionally restricted to
/// lowercase ASCII so content identities do not change meaning across platforms
/// or filesystems. A key is only constructible through [`ContentKey::parse`] or
/// [`TryFrom<&str>`], so every instance satisfies the same length and character
/// invariants.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ContentKey {
    namespace: String,
    path: String,
}

impl ContentKey {
    /// Parse and validate a namespaced content key.
    ///
    /// The parser validates the total length first to avoid doing unnecessary
    /// work on hostile input, then validates the namespace and path separately.
    /// The path may contain single `/` separators, but leading, trailing, and
    /// repeated separators are rejected. Those forms are rejected rather than
    /// normalized so two different input strings can never silently resolve to
    /// the same persistent identity.
    pub fn parse(value: &str) -> Result<Self, ContentKeyError> {
        if value.len() > MAX_CONTENT_KEY_LEN {
            return Err(ContentKeyError::TooLong {
                actual: value.len(),
                max: MAX_CONTENT_KEY_LEN,
            });
        }

        // Exactly one colon is required by the public format. `split_once`
        // keeps any additional colons in the path, where they are then rejected
        // by the path character check.
        let (namespace, path) = value
            .split_once(':')
            .ok_or(ContentKeyError::MissingNamespace)?;
        if namespace.is_empty() {
            return Err(ContentKeyError::EmptyNamespace);
        }
        if namespace.len() > MAX_NAMESPACE_LEN {
            return Err(ContentKeyError::NamespaceTooLong {
                actual: namespace.len(),
                max: MAX_NAMESPACE_LEN,
            });
        }
        if path.is_empty() {
            return Err(ContentKeyError::EmptyPath);
        }
        if path.len() > MAX_PATH_LEN {
            return Err(ContentKeyError::PathTooLong {
                actual: path.len(),
                max: MAX_PATH_LEN,
            });
        }
        if !namespace.bytes().all(is_namespace_byte) {
            return Err(ContentKeyError::InvalidNamespace);
        }
        if !path.bytes().all(is_path_byte) {
            return Err(ContentKeyError::InvalidPath);
        }
        if path.starts_with('/') || path.ends_with('/') || path.contains("//") {
            return Err(ContentKeyError::InvalidPath);
        }

        Ok(Self {
            namespace: namespace.to_owned(),
            path: path.to_owned(),
        })
    }

    /// Return the package or built-in domain portion of the key.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Return the slash-separated content path within the namespace.
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Display for ContentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.path)
    }
}

impl TryFrom<&str> for ContentKey {
    type Error = ContentKeyError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

fn is_namespace_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
}

fn is_path_byte(byte: u8) -> bool {
    is_namespace_byte(byte) || byte == b'/'
}

/// Errors produced while validating a content identity.
///
/// Variants carry the observed and maximum lengths where useful so diagnostics
/// can point at the exact validation failure instead of returning a generic
/// "invalid name" message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContentKeyError {
    TooLong { actual: usize, max: usize },
    MissingNamespace,
    EmptyNamespace,
    NamespaceTooLong { actual: usize, max: usize },
    EmptyPath,
    PathTooLong { actual: usize, max: usize },
    InvalidNamespace,
    InvalidPath,
}

impl fmt::Display for ContentKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong { actual, max } => {
                write!(f, "content key is {actual} bytes, limit is {max}")
            }
            Self::MissingNamespace => write!(f, "content key must contain `namespace:path`"),
            Self::EmptyNamespace => write!(f, "content namespace is empty"),
            Self::NamespaceTooLong { actual, max } => {
                write!(f, "content namespace is {actual} bytes, limit is {max}")
            }
            Self::EmptyPath => write!(f, "content path is empty"),
            Self::PathTooLong { actual, max } => {
                write!(f, "content path is {actual} bytes, limit is {max}")
            }
            Self::InvalidNamespace => {
                write!(f, "content namespace contains an unsupported character")
            }
            Self::InvalidPath => write!(f, "content path contains an unsupported character"),
        }
    }
}

impl std::error::Error for ContentKeyError {}

/// Stable world-local object identity used by persistence and cross-object references.
///
/// This identifier is never reused. Runtime code may cache it, but must not
/// derive it from a slot index, pointer, or process-local allocation address.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct EntityId(pub u128);

impl EntityId {
    /// First legal persistent object identifier. Zero is reserved as invalid.
    pub const FIRST: Self = Self(1);

    /// Verify that this identifier belongs to the legal persistent ID space.
    pub fn validate(self) -> Result<(), EntityIdError> {
        if self.0 == 0 {
            Err(EntityIdError::Zero)
        } else {
            Ok(())
        }
    }
}

/// Failure produced when a persistent object identifier is illegal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityIdError {
    /// Persistent identifiers start at one; zero is a sentinel, not an object.
    Zero,
}

impl fmt::Display for EntityIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => write!(f, "entity ids start at 1"),
        }
    }
}

impl std::error::Error for EntityIdError {}

/// Ephemeral object handle. Never persist this value as an object reference.
///
/// A handle is valid only for one slot lifetime. Reusing a slot must increment
/// `generation`; consumers compare both fields before dereferencing the slot.
/// This makes a stale task fail closed instead of observing a newly allocated
/// object that happens to occupy the same index.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct EntityHandle {
    /// Index into the runtime handle table.
    pub slot: u32,
    /// Slot generation observed when the handle was issued.
    pub generation: u32,
}
