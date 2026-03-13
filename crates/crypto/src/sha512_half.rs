use sha2::{Digest, Sha512};

use rxrpl_primitives::Hash256;

/// Compute SHA-512 of the concatenated inputs and return the first 32 bytes.
/// This is the core hashing primitive used throughout XRPL.
pub fn sha512_half(data: &[&[u8]]) -> Hash256 {
    let mut hasher = Sha512::new();
    for chunk in data {
        hasher.update(chunk);
    }
    let hash = hasher.finalize();
    let mut result = [0u8; 32];
    result.copy_from_slice(&hash[..32]);
    Hash256(result)
}

/// Convenience: SHA-512/2 of a single slice.
pub fn sha512_half_single(data: &[u8]) -> Hash256 {
    sha512_half(&[data])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha512_half_basic() {
        let result = sha512_half(&[b"test"]);
        assert_eq!(result.as_bytes().len(), 32);
        assert!(!result.is_zero());
    }

    #[test]
    fn sha512_half_multiple_inputs() {
        let combined = sha512_half(&[b"hello", b" ", b"world"]);
        let single = sha512_half(&[b"hello world"]);
        assert_eq!(combined, single);
    }
}
