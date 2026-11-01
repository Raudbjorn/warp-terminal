use super::*;

fn utf16(text: &str, bom: &[u8], bytes: fn(u16) -> [u8; 2]) -> Vec<u8> {
    let mut encoded = bom.to_vec();
    encoded.extend(text.encode_utf16().flat_map(bytes));
    encoded
}

#[test]
fn plain_utf8_round_trips() {
    for text in [
        "",
        "héllo\nwörld\n",
        "no newline",
        "crlf\r\nlines\r\n",
        "🦀",
    ] {
        let decoded = decode(text.as_bytes()).unwrap();
        assert_eq!(decoded.encoding, Encoding::Utf8);
        assert_eq!(decoded.text, text);
        assert_eq!(encode(&decoded.text, decoded.encoding), text.as_bytes());
    }
}

#[test]
fn the_byte_order_mark_is_kept_out_of_the_text_and_put_back() {
    let bytes = [UTF8_BOM, "héllo\r\n".as_bytes()].concat();
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded.encoding, Encoding::Utf8WithBom);
    assert_eq!(decoded.text, "héllo\r\n");
    assert_eq!(encode(&decoded.text, decoded.encoding), bytes);
}

#[test]
fn utf16_round_trips_in_both_byte_orders() {
    let text = "héllo 🦀\r\nwörld";
    let little = utf16(text, UTF16_LE_BOM, u16::to_le_bytes);
    let big = utf16(text, UTF16_BE_BOM, u16::to_be_bytes);

    let decoded = decode(&little).unwrap();
    assert_eq!(decoded.encoding, Encoding::Utf16Le);
    assert_eq!(decoded.text, text);
    assert_eq!(encode(&decoded.text, decoded.encoding), little);

    let decoded = decode(&big).unwrap();
    assert_eq!(decoded.encoding, Encoding::Utf16Be);
    assert_eq!(decoded.text, text);
    assert_eq!(encode(&decoded.text, decoded.encoding), big);
}

#[test]
fn binary_content_is_refused() {
    assert_eq!(decode(b"ELF\0\x01\x02"), Err(DecodeError::Binary));
    assert_eq!(decode(b"text\0text"), Err(DecodeError::Binary));
    assert_eq!(decode(&[0xFF, 0x00, 0xD8, 0xFF]), Err(DecodeError::Binary));
    // UTF-32 starts like UTF-16 and decodes to text full of NULs.
    assert_eq!(
        decode(&[0xFF, 0xFE, 0x00, 0x00, b'a', 0x00, 0x00, 0x00]),
        Err(DecodeError::Binary)
    );
}

#[test]
fn other_encodings_are_refused() {
    // Latin-1 "café".
    assert_eq!(
        decode(&[b'c', b'a', b'f', 0xE9]),
        Err(DecodeError::UnsupportedEncoding)
    );
    // UTF-16 with an odd number of bytes, and with an unpaired surrogate.
    assert_eq!(
        decode(&[0xFF, 0xFE, b'a', 0x00, b'b']),
        Err(DecodeError::UnsupportedEncoding)
    );
    assert_eq!(
        decode(&[0xFF, 0xFE, 0x00, 0xD8, b'a', 0x00]),
        Err(DecodeError::UnsupportedEncoding)
    );
}
