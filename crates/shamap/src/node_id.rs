use rxrpl_primitives::Hash256;

/// Maximum depth of the SHAMap tree (64 nibbles for 256-bit keys).
pub const MAX_DEPTH: u8 = 64;

/// Select which branch (0-15) a key follows at a given depth.
///
/// Each depth level corresponds to one hex nibble of the key.
/// Even depths use the upper nibble, odd depths use the lower nibble.
pub fn select_branch(key: &Hash256, depth: u8) -> u8 {
    let byte = key.as_bytes()[(depth / 2) as usize];
    if depth & 1 == 0 {
        byte >> 4 // upper nibble
    } else {
        byte & 0x0F // lower nibble
    }
}

/// A position in the SHAMap tree.
///
/// Tracks depth (0-64) and the key prefix up to that depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeID {
    depth: u8,
    id: Hash256,
}

impl NodeID {
    /// The root node (depth 0).
    pub const ROOT: Self = Self {
        depth: 0,
        id: Hash256::ZERO,
    };

    /// Create a NodeID for a specific key at a given depth.
    pub fn new(depth: u8, key: &Hash256) -> Self {
        debug_assert!(depth <= MAX_DEPTH);
        let mut id = [0u8; 32];
        let full_bytes = (depth / 2) as usize;
        id[..full_bytes].copy_from_slice(&key.as_bytes()[..full_bytes]);

        // Handle partial byte for odd depth
        if depth & 1 == 1 && full_bytes < 32 {
            id[full_bytes] = key.as_bytes()[full_bytes] & 0xF0;
        }

        Self {
            depth,
            id: Hash256::new(id),
        }
    }

    /// Return the depth of this node.
    pub fn depth(&self) -> u8 {
        self.depth
    }

    /// Return the id (masked key prefix).
    pub fn id(&self) -> &Hash256 {
        &self.id
    }

    /// Return true if this is the root node.
    pub fn is_root(&self) -> bool {
        self.depth == 0
    }

    /// Compute the child NodeID for a given branch.
    pub fn child(&self, branch: u8, key: &Hash256) -> Self {
        debug_assert!(branch < 16);
        debug_assert!(self.depth < MAX_DEPTH);
        Self::new(self.depth + 1, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn root_node_id() {
        assert!(NodeID::ROOT.is_root());
        assert_eq!(NodeID::ROOT.depth(), 0);
    }

    #[test]
    fn select_branch_upper_nibble() {
        // Key starts with 0xAB...
        let key = Hash256::from_str(
            "AB00000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        assert_eq!(select_branch(&key, 0), 0xA); // upper nibble of first byte
        assert_eq!(select_branch(&key, 1), 0xB); // lower nibble of first byte
    }

    #[test]
    fn select_branch_second_byte() {
        let key = Hash256::from_str(
            "00CD000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        assert_eq!(select_branch(&key, 2), 0xC); // upper nibble of second byte
        assert_eq!(select_branch(&key, 3), 0xD); // lower nibble of second byte
    }

    #[test]
    fn node_id_masking() {
        let key = Hash256::from_str(
            "ABCDEF0123456789000000000000000000000000000000000000000000000000",
        )
        .unwrap();

        let n1 = NodeID::new(1, &key);
        // Depth 1: only upper nibble of first byte
        assert_eq!(n1.id().as_bytes()[0], 0xA0);
        assert_eq!(n1.id().as_bytes()[1], 0x00);

        let n2 = NodeID::new(2, &key);
        // Depth 2: full first byte
        assert_eq!(n2.id().as_bytes()[0], 0xAB);
        assert_eq!(n2.id().as_bytes()[1], 0x00);

        let n4 = NodeID::new(4, &key);
        // Depth 4: first two bytes
        assert_eq!(n4.id().as_bytes()[0], 0xAB);
        assert_eq!(n4.id().as_bytes()[1], 0xCD);
    }

    #[test]
    fn child_advances_depth() {
        let key = Hash256::from_str(
            "ABCDEF0123456789000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let root = NodeID::ROOT;
        let child = root.child(0xA, &key);
        assert_eq!(child.depth(), 1);
    }
}
