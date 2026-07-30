//! Wire encoders for headless backend requests and artifacts.

pub(super) fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

pub(super) fn hex_hash(value: &[u8]) -> String {
    crate::domain::hash::sha256_hex(value)
}
