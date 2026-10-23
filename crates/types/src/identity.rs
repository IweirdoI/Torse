//! Stable content and object identities shared by engine boundaries.

use core::fmt;

const MAX_NAMESPACE_LEN: usize = 64;
const MAX_PATH_LEN: usize = 128;
const MAX_CONTENT_KEY_LEN: usize = MAX_NAMESPACE_LEN + 1 + MAX_PATH_LEN;

/// Fully qualified content identity such as `base:stone`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ContentKey {
    namespace: String,
    path: String,
}

impl ContentKey {
    /// Parse and validate a namespaced content key.
    pub fn parse(value: &str) -> Result<Self, ContentKeyError> {
        if value.len() > MAX_CONTENT_KEY_LEN {
            return Err(ContentKeyError::TooLong {
                actual: value.len(),
                max: MAX_CONTENT_KEY_LEN,
            });
        }

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

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct EntityId(pub u128);

impl EntityId {
    pub const FIRST: Self = Self(1);

    pub fn validate(self) -> Result<(), EntityIdError> {
        if self.0 == 0 {
            Err(EntityIdError::Zero)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityIdError {
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct EntityHandle {
    pub slot: u32,
    pub generation: u32,
}
