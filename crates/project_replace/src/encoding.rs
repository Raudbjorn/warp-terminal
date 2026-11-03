//! Reading a file as the text the search sees, and writing changed text back in the file's own
//! encoding.

use thiserror::Error;

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16_BE_BOM: &[u8] = &[0xFE, 0xFF];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    /// UTF-8 behind a byte order mark.
    Utf8WithBom,
    /// UTF-16 behind a byte order mark, little endian.
    Utf16Le,
    /// UTF-16 behind a byte order mark, big endian.
    Utf16Be,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("the file is binary")]
    Binary,
    #[error("the file is not valid UTF-8 or UTF-16")]
    UnsupportedEncoding,
}

/// The text of a file without its byte order mark, and the encoding to write it back in.
///
/// Decoding then encoding gives back the original bytes exactly, which is what lets a replace
/// leave everything it does not touch alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedText {
    pub text: String,
    pub encoding: Encoding,
}

pub fn decode(bytes: &[u8]) -> Result<DecodedText, DecodeError> {
    let (text, encoding) = if let Some(body) = bytes.strip_prefix(UTF8_BOM) {
        (decode_utf8(body)?, Encoding::Utf8WithBom)
    } else if let Some(body) = bytes.strip_prefix(UTF16_LE_BOM) {
        (decode_utf16(body, u16::from_le_bytes)?, Encoding::Utf16Le)
    } else if let Some(body) = bytes.strip_prefix(UTF16_BE_BOM) {
        (decode_utf16(body, u16::from_be_bytes)?, Encoding::Utf16Be)
    } else {
        (decode_utf8(bytes)?, Encoding::Utf8)
    };

    // The search stops at the first NUL, so the rest of such a file was never searched.
    if text.contains('\0') {
        return Err(DecodeError::Binary);
    }
    Ok(DecodedText { text, encoding })
}

fn decode_utf8(bytes: &[u8]) -> Result<String, DecodeError> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text.to_string()),
        Err(_) if bytes.contains(&0) => Err(DecodeError::Binary),
        Err(_) => Err(DecodeError::UnsupportedEncoding),
    }
}

fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Result<String, DecodeError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(DecodeError::UnsupportedEncoding);
    }
    let units = bytes.chunks_exact(2).map(|pair| unit([pair[0], pair[1]]));
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map_err(|_| DecodeError::UnsupportedEncoding)
}

pub fn encode(text: &str, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Utf8WithBom => [UTF8_BOM, text.as_bytes()].concat(),
        Encoding::Utf16Le => encode_utf16(text, UTF16_LE_BOM, u16::to_le_bytes),
        Encoding::Utf16Be => encode_utf16(text, UTF16_BE_BOM, u16::to_be_bytes),
    }
}

fn encode_utf16(text: &str, bom: &[u8], bytes: fn(u16) -> [u8; 2]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(bom.len() + text.len() * 2);
    encoded.extend_from_slice(bom);
    encoded.extend(text.encode_utf16().flat_map(bytes));
    encoded
}

#[cfg(test)]
#[path = "encoding_tests.rs"]
mod tests;
