//! Hardware-independent ESP-SR model-pack validation tests.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    const fn from_hex(hex: &[u8; 64]) -> Self {
        let mut bytes = [0_u8; 32];
        let mut index = 0;
        while index < bytes.len() {
            bytes[index] = (hex_nibble(hex[index * 2]) << 4) | hex_nibble(hex[index * 2 + 1]);
            index += 1;
        }
        Self(bytes)
    }
}

const fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

#[derive(Debug)]
enum SrError {
    ErasedPack,
    InvalidPack(String),
    InternalInvariant(&'static str),
}

#[path = "../../../src/sr/fixture.rs"]
mod fixture;
