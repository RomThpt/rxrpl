use rxrpl_primitives::Hash256;

/// Number of children per inner node.
pub const BRANCH_FACTOR: usize = 16;

/// Maximum depth of the SHAMap tree (64 nibbles for 256-bit keys).
pub const MAX_DEPTH: u8 = 64;

/// Select which branch (0-15) a key follows at a given depth.
///
/// Each depth level corresponds to one hex nibble of the key.
/// Even depths use the upper nibble, odd depths use the lower nibble.
pub fn select_branch(key: &Hash256, depth: u8) -> u8 {
    let byte = key.as_bytes()[(depth / 2) as usize];
    if depth & 1 == 0 {
        byte >> 4
    } else {
        byte & 0x0F
    }
}

/// A position in the SHAMap tree.
///
/// Tracks depth (0-64) and the key prefix masked to that depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId {
    depth: u8,
    id: Hash256,
}

impl NodeId {
    /// The root node (depth 0).
    pub const ROOT: Self = Self {
        depth: 0,
        id: Hash256::ZERO,
    };

    /// Create a NodeId for a specific key at a given depth.
    pub fn new(depth: u8, key: &Hash256) -> Self {
        debug_assert!(depth <= MAX_DEPTH);
        let mut id = [0u8; 32];
        let full_bytes = (depth / 2) as usize;
        id[..full_bytes].copy_from_slice(&key.as_bytes()[..full_bytes]);

        if depth & 1 == 1 && full_bytes < 32 {
            id[full_bytes] = key.as_bytes()[full_bytes] & 0xF0;
        }

        Self {
            depth,
            id: Hash256::new(id),
        }
    }

    pub fn depth(&self) -> u8 {
        self.depth
    }

    pub fn id(&self) -> &Hash256 {
        &self.id
    }

    pub fn is_root(&self) -> bool {
        self.depth == 0
    }

    /// Compute the child NodeId for a given branch.
    pub fn child(&self, _branch: u8, key: &Hash256) -> Self {
        debug_assert!(_branch < 16);
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
        assert!(NodeId::ROOT.is_root());
        assert_eq!(NodeId::ROOT.depth(), 0);
    }

    #[test]
    fn select_branch_upper_nibble() {
        let key = Hash256::from_str(
            "AB00000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        assert_eq!(select_branch(&key, 0), 0xA);
        assert_eq!(select_branch(&key, 1), 0xB);
    }

    #[test]
    fn select_branch_second_byte() {
        let key = Hash256::from_str(
            "00CD000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        assert_eq!(select_branch(&key, 2), 0xC);
        assert_eq!(select_branch(&key, 3), 0xD);
    }

    #[test]
    fn node_id_masking() {
        let key = Hash256::from_str(
            "ABCDEF0123456789000000000000000000000000000000000000000000000000",
        )
        .unwrap();

        let n1 = NodeId::new(1, &key);
        assert_eq!(n1.id().as_bytes()[0], 0xA0);
        assert_eq!(n1.id().as_bytes()[1], 0x00);

        let n2 = NodeId::new(2, &key);
        assert_eq!(n2.id().as_bytes()[0], 0xAB);
        assert_eq!(n2.id().as_bytes()[1], 0x00);

        let n4 = NodeId::new(4, &key);
        assert_eq!(n4.id().as_bytes()[0], 0xAB);
        assert_eq!(n4.id().as_bytes()[1], 0xCD);
    }

    #[test]
    fn child_advances_depth() {
        let key = Hash256::from_str(
            "ABCDEF0123456789000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let root = NodeId::ROOT;
        let child = root.child(0xA, &key);
        assert_eq!(child.depth(), 1);
    }

    #[test]
    fn select_branch_all_nibbles() {
        let key = Hash256::from_str(
            "0123456789ABCDEF000000000000000000000000000000000000000000000000",
        )
        .unwrap();

        assert_eq!(select_branch(&key, 0), 0x0);
        assert_eq!(select_branch(&key, 1), 0x1);
        assert_eq!(select_branch(&key, 2), 0x2);
        assert_eq!(select_branch(&key, 3), 0x3);
        assert_eq!(select_branch(&key, 4), 0x4);
        assert_eq!(select_branch(&key, 5), 0x5);
        assert_eq!(select_branch(&key, 6), 0x6);
        assert_eq!(select_branch(&key, 7), 0x7);
        assert_eq!(select_branch(&key, 8), 0x8);
        assert_eq!(select_branch(&key, 9), 0x9);
        assert_eq!(select_branch(&key, 10), 0xA);
        assert_eq!(select_branch(&key, 11), 0xB);
        assert_eq!(select_branch(&key, 12), 0xC);
        assert_eq!(select_branch(&key, 13), 0xD);
        assert_eq!(select_branch(&key, 14), 0xE);
        assert_eq!(select_branch(&key, 15), 0xF);
    }
}
