//! Locating the `raw` element inside the UBJSON file framing.

use crate::ReplayError;

/// `{` opens the root object, then key `U 3 "raw"`, then an optimized array
/// header: `[` of type `$U` (uint8) with a `#l` (int32) count.
const RAW_HEADER: &[u8] = b"{U\x03raw[$U#l";

/// Returns the bytes of the `raw` element (the event stream), ignoring the
/// `metadata` element that follows it.
pub fn raw_element(file: &[u8]) -> Result<&[u8], ReplayError> {
    let Some(rest) = file.strip_prefix(RAW_HEADER) else {
        return Err(if !file.is_empty() && RAW_HEADER.starts_with(file) {
            ReplayError::Truncated
        } else {
            ReplayError::NotSlp
        });
    };
    let (len, stream) = rest
        .split_first_chunk::<4>()
        .ok_or(ReplayError::Truncated)?;
    let len = usize::try_from(u32::from_be_bytes(*len)).map_err(|_| ReplayError::Truncated)?;
    stream.get(..len).ok_or(ReplayError::Truncated)
}
