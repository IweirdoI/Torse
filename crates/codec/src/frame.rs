//! Length-prefixed frames that wrap one complete record.

use crate::{Decoder, EncodeResult, Encoder};

/// Write a frame bounded by the configured frame limit. Nothing is appended on failure.
pub fn write_frame(encoder: &mut Encoder, payload: &[u8]) -> EncodeResult<()> {
    let max = encoder.limits().max_frame_bytes;
    encoder.write_prefixed(payload, max, "frame")
}

/// Read a frame, rejecting declared lengths above the configured frame limit.
pub fn read_frame<'a>(decoder: &mut Decoder<'a>) -> EncodeResult<&'a [u8]> {
    let max = decoder.limits().max_frame_bytes;
    decoder.read_prefixed(max, "frame")
}
