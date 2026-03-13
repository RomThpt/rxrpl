use rxrpl_primitives::Hash256;

/// A key-value pair stored in a SHAMap leaf node.
///
/// The key is a Hash256 (keylet hash for state entries, or tx hash for tx entries).
/// The data is the serialized object bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SHAMapItem {
    key: Hash256,
    data: Vec<u8>,
}

impl SHAMapItem {
    pub fn new(key: Hash256, data: Vec<u8>) -> Self {
        Self { key, data }
    }

    pub fn key(&self) -> &Hash256 {
        &self.key
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_roundtrip() {
        let key = Hash256::new([0xAA; 32]);
        let data = vec![1, 2, 3, 4];
        let item = SHAMapItem::new(key, data.clone());
        assert_eq!(*item.key(), key);
        assert_eq!(item.data(), &data[..]);
    }
}
