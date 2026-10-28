//! Stable resource identities and portable pack path syntax.

/// Assigned identity independent of source bytes and runtime residency.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct AssetGuid(pub u128);

/// The shared path grammar used by resource aliases and cooked metadata.
pub fn valid_asset_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.contains("//")
        && !value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b'.' | b'/')
        })
}
