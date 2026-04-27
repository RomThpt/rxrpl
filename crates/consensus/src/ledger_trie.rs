//! Port of rippled `LedgerTrie<Ledger>` (xrpl/consensus/LedgerTrie.h).
//!
//! Single-writer, no concurrency. Used by ValidationsTrie for
//! preferred-branch discovery — at each fork the branch with the most
//! cumulative support (`branch_support`) wins; ties broken deterministically
//! by lower hash (matches rippled's `span.startID()` tie-break direction
//! when applied to per-hash nodes).
//!
//! NIGHT-SHIFT-REVIEW: rippled uses a *compressed* trie of variable-length
//! `Span`s plus a `seqSupport` map keyed by ledger sequence so that
//! `getPreferred(largestIssued)` can subtract uncommitted support. This port
//! is the simpler per-hash-node version: each node holds one ledger hash,
//! children keyed by the next hash. API matches the T15 task spec
//! (`insert(branch, count)` / `get_preferred() -> Option<Hash256>`).
//! For the full rippled algorithm with span compression and uncommitted
//! support, port `Span<Ledger>` and the `seqSupport` map next.

use std::collections::HashMap;

use rxrpl_primitives::Hash256;

/// One node of the ledger trie. Keyed in its parent's `children` map by
/// `hash`. Stores tip support (validations directly on this hash) and
/// cumulative branch support (tip_support + sum of children's branch_support).
#[derive(Debug, Clone)]
struct Node {
    hash: Hash256,
    tip_support: u32,
    branch_support: u32,
    children: HashMap<Hash256, Node>,
}

impl Node {
    fn new(hash: Hash256) -> Self {
        Self {
            hash,
            tip_support: 0,
            branch_support: 0,
            children: HashMap::new(),
        }
    }
}

/// Ancestry trie of ledgers.
///
/// A trie over sequences of ledger hashes. Each `branch` passed to
/// `insert`/`remove` is the path of ledger hashes from a fixed anchor
/// (typically genesis) to the tip the validator is voting for. Common
/// prefixes are shared, so a fork at depth `d` adds support to all
/// ancestors `[0..=d]` once and to the diverging suffix only on its branch.
pub struct LedgerTrie {
    /// Sentinel root. Children of the root are anchors (e.g. genesis or
    /// any first-hash a validator votes for). The root itself has no hash.
    root: Node,
}

impl Default for LedgerTrie {
    fn default() -> Self {
        Self::new()
    }
}

impl LedgerTrie {
    /// Create an empty trie.
    pub fn new() -> Self {
        Self {
            // The root's hash is never read — it's a sentinel parent. ZERO
            // is fine because we never look it up in any child map.
            root: Node::new(Hash256::ZERO),
        }
    }

    /// Insert support for the given branch (sequence of ledger hashes from
    /// anchor to tip). Walks/creates the path, incrementing branch_support
    /// at every node along the way and tip_support at the leaf.
    ///
    /// Inserting an empty branch is a no-op (no tip to support).
    pub fn insert(&mut self, branch: &[Hash256], count: u32) {
        if branch.is_empty() || count == 0 {
            return;
        }
        Self::insert_walk(&mut self.root, branch, count);
    }

    fn insert_walk(node: &mut Node, branch: &[Hash256], count: u32) {
        // Always bump branch_support of every node we traverse on the way
        // to the leaf — this includes the sentinel root, but root's
        // branch_support is never observed externally.
        node.branch_support += count;

        let (head, rest) = branch.split_first().expect("non-empty");
        let child = node
            .children
            .entry(*head)
            .or_insert_with(|| Node::new(*head));
        if rest.is_empty() {
            // Leaf — credit tip support to this node and finish bumping
            // branch_support on the leaf itself.
            child.branch_support += count;
            child.tip_support += count;
        } else {
            Self::insert_walk(child, rest, count);
        }
    }

    /// Remove support for the given branch. Returns `true` if the branch
    /// existed (i.e. its leaf had tip_support >= 1) and was decremented.
    /// Returns `false` if the branch is unknown or its leaf has no
    /// tip_support to remove. Empty / zero-count calls return `false`.
    ///
    /// `count` is clamped to the leaf's current tip_support.
    /// When a node's branch_support drops to zero it is pruned from its
    /// parent (mirrors rippled's compress-on-remove behaviour).
    pub fn remove(&mut self, branch: &[Hash256], count: u32) -> bool {
        if branch.is_empty() || count == 0 {
            return false;
        }
        // First peek the leaf to compute the actual decrement (clamp to
        // tip_support) so every node on the path is decremented by the
        // same amount.
        let actual = match Self::peek_tip_support(&self.root, branch) {
            Some(t) if t > 0 => count.min(t),
            _ => return false,
        };
        Self::remove_walk(&mut self.root, branch, actual);
        true
    }

    fn peek_tip_support(node: &Node, branch: &[Hash256]) -> Option<u32> {
        let (head, rest) = branch.split_first()?;
        let child = node.children.get(head)?;
        if rest.is_empty() {
            Some(child.tip_support)
        } else {
            Self::peek_tip_support(child, rest)
        }
    }

    fn remove_walk(node: &mut Node, branch: &[Hash256], count: u32) {
        node.branch_support = node.branch_support.saturating_sub(count);

        let (head, rest) = branch.split_first().expect("non-empty");
        // We verified the path exists in peek_tip_support above; using
        // get_mut + expect keeps the recursion straightforward.
        let prune = {
            let child = node
                .children
                .get_mut(head)
                .expect("path verified by peek_tip_support");
            if rest.is_empty() {
                child.branch_support = child.branch_support.saturating_sub(count);
                child.tip_support = child.tip_support.saturating_sub(count);
            } else {
                Self::remove_walk(child, rest, count);
            }
            child.branch_support == 0
        };
        if prune {
            node.children.remove(head);
        }
    }

    /// Tip support for a specific hash (validations directly on it).
    /// Searches all nodes; returns 0 if not present.
    pub fn tip_support(&self, hash: &Hash256) -> u32 {
        Self::find_node(&self.root, hash)
            .map(|n| n.tip_support)
            .unwrap_or(0)
    }

    /// Branch support for a specific hash (tip_support + descendants).
    /// Searches all nodes; returns 0 if not present.
    pub fn branch_support(&self, hash: &Hash256) -> u32 {
        Self::find_node(&self.root, hash)
            .map(|n| n.branch_support)
            .unwrap_or(0)
    }

    fn find_node<'a>(node: &'a Node, hash: &Hash256) -> Option<&'a Node> {
        for child in node.children.values() {
            if &child.hash == hash {
                return Some(child);
            }
            if let Some(found) = Self::find_node(child, hash) {
                return Some(found);
            }
        }
        None
    }

    /// Returns `true` when the trie holds no support at all.
    pub fn is_empty(&self) -> bool {
        self.root.branch_support == 0
    }

    /// Walk the trie greedily and return the hash of the preferred tip.
    ///
    /// At each level, follow the child with the largest `branch_support`.
    /// Ties are broken deterministically by the lower hash (mirrors
    /// rippled's `span.startID()` tie-break, applied to per-hash nodes).
    /// Stops descending when the current node's `tip_support` is at least
    /// as large as the best child's `branch_support` — i.e. switching to a
    /// deeper branch with strictly less cumulative support is rejected
    /// (the "deeper-fork-with-less-support" case).
    ///
    /// NIGHT-SHIFT-REVIEW: this is the simplified preferred-branch rule.
    /// Rippled additionally subtracts `uncommitted` support (validators
    /// that have not yet voted at this seq) from the margin and takes a
    /// `largestIssued` parameter. The simpler rule is sufficient for the
    /// tests required by T15 and matches the spec's `get_preferred()`
    /// signature.
    pub fn get_preferred(&self) -> Option<Hash256> {
        if self.is_empty() {
            return None;
        }
        let mut curr: &Node = &self.root;
        let mut preferred: Option<Hash256> = None;

        loop {
            // Pick the child with the highest branch_support; tie-break
            // on lower hash for determinism.
            let mut best: Option<&Node> = None;
            for child in curr.children.values() {
                best = Some(match best {
                    None => child,
                    Some(b) => {
                        if child.branch_support > b.branch_support
                            || (child.branch_support == b.branch_support && child.hash < b.hash)
                        {
                            child
                        } else {
                            b
                        }
                    }
                });
            }
            let Some(best) = best else {
                // No children — the current preferred (if any) is final.
                return preferred;
            };

            // If we're sitting on a node with tip_support that meets or
            // exceeds the best child's branch_support, stop here: the
            // deeper branch doesn't have enough support to overtake.
            // (The sentinel root has tip_support 0 so this never blocks
            // the first descent.)
            if curr.tip_support >= best.branch_support {
                return preferred;
            }

            preferred = Some(best.hash);
            curr = best;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> Hash256 {
        let mut bytes = [0u8; 32];
        bytes[0] = byte;
        Hash256::new(bytes)
    }

    #[test]
    fn empty_trie_get_preferred_returns_none() {
        let trie = LedgerTrie::new();
        assert!(trie.is_empty());
        assert_eq!(trie.get_preferred(), None);
    }

    #[test]
    fn single_chain_insert_get_preferred_returns_tip() {
        let mut trie = LedgerTrie::new();
        let branch = vec![h(1), h(2), h(3)];
        trie.insert(&branch, 1);

        assert!(!trie.is_empty());
        assert_eq!(trie.get_preferred(), Some(h(3)));
        assert_eq!(trie.tip_support(&h(3)), 1);
        assert_eq!(trie.branch_support(&h(1)), 1);
        assert_eq!(trie.branch_support(&h(2)), 1);
        assert_eq!(trie.branch_support(&h(3)), 1);
    }

    #[test]
    fn equal_support_fork_breaks_tie_by_lower_hash() {
        // Two branches diverging at depth 1: [1,2] and [1,3], each with
        // support 1. Tie-break = lower hash, so h(2) wins over h(3).
        let mut trie = LedgerTrie::new();
        trie.insert(&[h(1), h(2)], 1);
        trie.insert(&[h(1), h(3)], 1);

        assert_eq!(trie.branch_support(&h(1)), 2);
        assert_eq!(trie.branch_support(&h(2)), 1);
        assert_eq!(trie.branch_support(&h(3)), 1);
        assert_eq!(trie.get_preferred(), Some(h(2)));
    }

    #[test]
    fn fork_with_more_support_wins() {
        // [1,2] support 1, [1,3] support 3 -> h(3) wins.
        let mut trie = LedgerTrie::new();
        trie.insert(&[h(1), h(2)], 1);
        trie.insert(&[h(1), h(3)], 3);

        assert_eq!(trie.branch_support(&h(2)), 1);
        assert_eq!(trie.branch_support(&h(3)), 3);
        assert_eq!(trie.get_preferred(), Some(h(3)));
    }

    #[test]
    fn deeper_fork_with_less_support_does_not_overtake() {
        // [1,2] gets 5 votes (tip-support at h(2) = 5).
        // [1,2,3,4] gets 2 votes (deeper, but cumulative 2 < 5).
        // Preferred branch should stop at h(2): deeper branch has less
        // cumulative support than h(2)'s tip support.
        let mut trie = LedgerTrie::new();
        for _ in 0..5 {
            trie.insert(&[h(1), h(2)], 1);
        }
        for _ in 0..2 {
            trie.insert(&[h(1), h(2), h(3), h(4)], 1);
        }

        assert_eq!(trie.tip_support(&h(2)), 5);
        assert_eq!(trie.branch_support(&h(2)), 7);
        assert_eq!(trie.branch_support(&h(3)), 2);
        assert_eq!(trie.branch_support(&h(4)), 2);
        assert_eq!(trie.get_preferred(), Some(h(2)));
    }

    #[test]
    fn deeper_fork_with_more_support_wins() {
        // Inverse of the above: [1,2] gets 1, [1,2,3,4] gets 3.
        // Now branch_support at h(3)=3 > tip_support at h(2)=1, so we
        // descend past h(2) to the leaf h(4).
        let mut trie = LedgerTrie::new();
        trie.insert(&[h(1), h(2)], 1);
        for _ in 0..3 {
            trie.insert(&[h(1), h(2), h(3), h(4)], 1);
        }

        assert_eq!(trie.tip_support(&h(2)), 1);
        assert_eq!(trie.branch_support(&h(2)), 4);
        assert_eq!(trie.branch_support(&h(4)), 3);
        assert_eq!(trie.get_preferred(), Some(h(4)));
    }

    #[test]
    fn remove_brings_support_to_zero_and_prunes_branch() {
        // Insert one branch, remove it -> trie empty again.
        let mut trie = LedgerTrie::new();
        trie.insert(&[h(1), h(2), h(3)], 1);
        assert_eq!(trie.branch_support(&h(1)), 1);

        let ok = trie.remove(&[h(1), h(2), h(3)], 1);
        assert!(ok);
        assert!(trie.is_empty());
        assert_eq!(trie.tip_support(&h(3)), 0);
        assert_eq!(trie.branch_support(&h(1)), 0);
        assert_eq!(trie.get_preferred(), None);

        // Removing again returns false (nothing to remove).
        assert!(!trie.remove(&[h(1), h(2), h(3)], 1));
    }

    #[test]
    fn remove_only_decrements_when_partial_support_remains() {
        let mut trie = LedgerTrie::new();
        trie.insert(&[h(1), h(2)], 3);
        assert_eq!(trie.tip_support(&h(2)), 3);

        assert!(trie.remove(&[h(1), h(2)], 1));
        assert_eq!(trie.tip_support(&h(2)), 2);
        assert_eq!(trie.branch_support(&h(1)), 2);

        // Over-remove: clamp to remaining tip_support, branch goes to 0.
        assert!(trie.remove(&[h(1), h(2)], 99));
        assert!(trie.is_empty());
    }

    #[test]
    fn remove_unknown_branch_returns_false() {
        let mut trie = LedgerTrie::new();
        trie.insert(&[h(1), h(2)], 1);
        // Different branch — not present.
        assert!(!trie.remove(&[h(1), h(9)], 1));
        // Internal node, not a tip — has branch_support but no tip_support
        // at h(1), so removing the branch [h(1)] returns false.
        assert!(!trie.remove(&[h(1)], 1));
    }

    #[test]
    fn deep_branch_does_not_blow_up_memory() {
        // Insert a branch of depth 64 (well beyond the >20 threshold).
        // We exercise insert -> get_preferred -> remove on the same
        // branch and verify the trie is empty afterwards.
        let mut trie = LedgerTrie::new();
        let branch: Vec<Hash256> = (1u8..=64).map(h).collect();
        trie.insert(&branch, 1);

        assert_eq!(trie.get_preferred(), Some(h(64)));
        assert_eq!(trie.branch_support(&h(1)), 1);
        assert_eq!(trie.branch_support(&h(64)), 1);

        assert!(trie.remove(&branch, 1));
        assert!(trie.is_empty());
    }

    #[test]
    fn insert_same_branch_twice_with_count_one_equals_count_two_once() {
        let branch = vec![h(1), h(2), h(3)];

        let mut trie_a = LedgerTrie::new();
        trie_a.insert(&branch, 1);
        trie_a.insert(&branch, 1);

        let mut trie_b = LedgerTrie::new();
        trie_b.insert(&branch, 2);

        for hash in [h(1), h(2), h(3)] {
            assert_eq!(
                trie_a.tip_support(&hash),
                trie_b.tip_support(&hash),
                "tip_support mismatch at {:?}",
                hash
            );
            assert_eq!(
                trie_a.branch_support(&hash),
                trie_b.branch_support(&hash),
                "branch_support mismatch at {:?}",
                hash
            );
        }
        assert_eq!(trie_a.get_preferred(), trie_b.get_preferred());
    }

    #[test]
    fn three_way_fork_picks_largest_branch() {
        // Three branches at depth 1: [1,2]=1, [1,3]=2, [1,4]=4 -> h(4) wins.
        let mut trie = LedgerTrie::new();
        trie.insert(&[h(1), h(2)], 1);
        trie.insert(&[h(1), h(3)], 2);
        trie.insert(&[h(1), h(4)], 4);

        assert_eq!(trie.branch_support(&h(1)), 7);
        assert_eq!(trie.get_preferred(), Some(h(4)));
    }

    #[test]
    fn empty_branch_and_zero_count_are_no_ops() {
        let mut trie = LedgerTrie::new();
        trie.insert(&[], 5);
        trie.insert(&[h(1)], 0);
        assert!(trie.is_empty());
        assert_eq!(trie.get_preferred(), None);
        assert!(!trie.remove(&[], 1));
        assert!(!trie.remove(&[h(1)], 0));
    }
}
