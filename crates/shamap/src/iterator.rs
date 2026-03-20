use std::sync::Arc;

use rxrpl_primitives::Hash256;

use crate::inner_node::InnerNode;
use crate::leaf_node::LeafNode;
use crate::node::SHAMapNode;
use crate::node_store::NodeStore;

/// Depth-first leaf iterator over a SHAMap.
///
/// Visits all leaves in branch order (0-15 at each level), which yields
/// leaves sorted by key since branch selection is based on key nibbles.
pub struct SHAMapIter {
    stack: Vec<(Arc<SHAMapNode>, u8)>,
    store: Option<Arc<dyn NodeStore>>,
    leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
}

impl SHAMapIter {
    pub(crate) fn new(
        root: Arc<SHAMapNode>,
        store: Option<Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) -> Self {
        SHAMapIter {
            stack: vec![(root, 0)],
            store,
            leaf_ctor,
        }
    }
}

impl Iterator for SHAMapIter {
    type Item = (Hash256, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (node, branch) = self.stack.last_mut()?;

            match node.as_ref() {
                SHAMapNode::Leaf(leaf) => {
                    let result = (*leaf.key(), leaf.data().to_vec());
                    self.stack.pop();
                    return Some(result);
                }
                SHAMapNode::Inner(inner) => {
                    let mut b = *branch;
                    while b < 16 && inner.is_empty_branch(b) {
                        b += 1;
                    }

                    if b >= 16 {
                        self.stack.pop();
                        continue;
                    }

                    *branch = b + 1;

                    // Use child_with_store for lazy loading, clone the Arc
                    let child = inner
                        .child_with_store(b, self.store.as_ref(), self.leaf_ctor)
                        .ok()
                        .flatten()
                        .cloned();
                    if let Some(child_arc) = child {
                        self.stack.push((child_arc, 0));
                    }
                }
            }
        }
    }
}

/// Borrowing iterator over a SHAMap, yielding references to leaf key/data.
pub struct SHAMapRefIter<'a> {
    stack: Vec<(&'a InnerNode, u8)>,
    store: Option<&'a Arc<dyn NodeStore>>,
    leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
}

impl<'a> SHAMapRefIter<'a> {
    pub(crate) fn new(
        root: &'a SHAMapNode,
        store: Option<&'a Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) -> Self {
        let mut iter = SHAMapRefIter {
            stack: Vec::new(),
            store,
            leaf_ctor,
        };
        if let SHAMapNode::Inner(inner) = root {
            iter.stack.push((inner, 0));
        }
        iter
    }
}

impl<'a> Iterator for SHAMapRefIter<'a> {
    type Item = (&'a Hash256, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (inner, branch) = self.stack.last_mut()?;

            let mut b = *branch;
            while b < 16 && inner.is_empty_branch(b) {
                b += 1;
            }

            if b >= 16 {
                self.stack.pop();
                continue;
            }

            *branch = b + 1;

            if let Ok(Some(child)) = inner.child_with_store(b, self.store, self.leaf_ctor) {
                match child.as_ref() {
                    SHAMapNode::Leaf(leaf) => {
                        return Some((leaf.key(), leaf.data()));
                    }
                    SHAMapNode::Inner(child_inner) => {
                        self.stack.push((child_inner, 0));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shamap::SHAMap;
    use std::str::FromStr;

    fn make_key(hex: &str) -> Hash256 {
        Hash256::from_str(hex).unwrap()
    }

    #[test]
    fn empty_iterator() {
        let map = SHAMap::account_state();
        let items: Vec<_> = map.iter().collect();
        assert!(items.is_empty());
    }

    #[test]
    fn single_item() {
        let mut map = SHAMap::account_state();
        let key = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        map.put(key, vec![1, 2, 3]).unwrap();

        let items: Vec<_> = map.iter().collect();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].0, key);
        assert_eq!(items[0].1, vec![1, 2, 3]);
    }

    #[test]
    fn multiple_items_sorted_by_key() {
        let mut map = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        let k3 = make_key("3000000000000000000000000000000000000000000000000000000000000000");

        map.put(k3, vec![3]).unwrap();
        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();

        let items: Vec<_> = map.iter().collect();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].0, k1);
        assert_eq!(items[1].0, k2);
        assert_eq!(items[2].0, k3);
    }

    #[test]
    fn ref_iterator() {
        let mut map = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");

        map.put(k1, vec![10]).unwrap();
        map.put(k2, vec![20]).unwrap();

        let items: Vec<_> = map.iter_ref().collect();
        assert_eq!(items.len(), 2);
        assert_eq!(*items[0].0, k1);
        assert_eq!(items[0].1, &[10]);
        assert_eq!(*items[1].0, k2);
        assert_eq!(items[1].1, &[20]);
    }

    #[test]
    fn many_items_iterator() {
        let mut map = SHAMap::account_state();
        for i in 0u8..50 {
            let mut key_bytes = [0u8; 32];
            key_bytes[0] = i;
            key_bytes[1] = i.wrapping_mul(37);
            map.put(Hash256::new(key_bytes), vec![i]).unwrap();
        }

        let items: Vec<_> = map.iter().collect();
        assert_eq!(items.len(), 50);

        for i in 1..items.len() {
            assert!(items[i - 1].0 < items[i].0);
        }
    }
}
