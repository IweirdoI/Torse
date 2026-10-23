//! Frame encoding utilities for structured binary data.
//!
//! Frames provide length-prefixed encoding for variable-sized data.

use crate::{Decoder, EncodeResult, Encoder};

/// Maximum frame size to prevent unbounded allocations (16 MB).
pub const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

/// Write a length-prefixed frame.
pub fn write_frame(encoder: &mut Encoder, data: &[u8]) -> EncodeResult<()> {
    let len = data.len();
    if len > MAX_FRAME_SIZE as usize {
        return Err(torse_types::Fault::resource_limit(format!(
            "frame size {} exceeds maximum {}",
            len, MAX_FRAME_SIZE
        )));
    }
    encoder.write_u32(len as u32)?;
    encoder.write(data)?;
    Ok(())
}

/// Read a length-prefixed frame.
pub fn read_frame<'a>(decoder: &mut Decoder<'a>) -> EncodeResult<&'a [u8]> {
    let len = decoder.read_u32()?;
    if len > MAX_FRAME_SIZE {
        return Err(torse_types::Fault::invalid_data(format!(
            "frame size {} exceeds maximum {}",
            len, MAX_FRAME_SIZE
        )));
    }
    decoder.read(len as usize)
}
