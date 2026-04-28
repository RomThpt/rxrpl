//! Port of rippled `LedgerTrie<Ledger>` (xrpl/consensus/LedgerTrie.h).
//!
//! Single-writer, no concurrency. Used by ValidationsTrie for
//! preferred-branch discovery — at each fork the branch with the most
//! cumulative support (`branch_support`) wins; ties broken deterministically
//! by HIGHER hash (matches rippled's `LedgerTrie.h` tie-break direction).
//!
//! DESIGN: rippled uses a *compressed* trie of variable-length `Span`s
//! plus a `seqSupport` map keyed by ledger sequence. rxrpl deliberately
//! ships the simpler per-hash-node form for two reasons:
//!
//! 1. Bounded width and depth. The trusted UNL is bounded (XRPL mainnet:
//!    ~35 validators; rxrpl target profile: ≤35) so the maximum branching
//!    factor at any node is ≤ N_unl. Validators feed at most one branch
//!    each via [`crate::validations_trie::ValidationsTrie::add_with_parents`],
//!    and the typical branch length is the in-flight ledger window
//!    (single-digits in steady state). Total node count stays
//!    O(N_unl · D_window) = a few hundred nodes; the per-call walk cost is
//!    negligible. Span compression in rippled targets the much larger
//!    historical trie that retains ledgers across many epochs — a use case
//!    we don't have here because [`Self::remove`] already prunes
//!    zero-support nodes on every decrement (see `remove_walk` below).
//! 2. Seq-based subtraction is handled one layer up. rippled's
//!    `getPreferred(largestSeq)` interleaves "drop validations whose seq <
//!    largestSeq" with the trie walk by maintaining a `seqSupport` map
//!    inside each node. rxrpl's
//!    [`crate::validations_trie::ValidationsTrie::get_preferred`] does the
//!    same filtering up front (audit fix H5): it iterates `self.latest`,
//!    rebuilds a transient trie over the fresh subset, and memoises the
//!    answer. The architectural split — content-addressed trie below,
//!    seq-aware aggregator above — keeps each layer single-purpose and
//!    avoids embedding sequence metadata in a structure that is otherwise
//!    purely about hash ancestry.

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
    /// Ties are broken deterministically by the HIGHER hash (matches
    /// rippled's `LedgerTrie.h` tie-break — see the `preferredChild`
    /// comparison that prefers the larger `span.startID()`).
    /// Stops descending when the current node's `tip_support` is at least
    /// as large as the best child's `branch_support` — i.e. switching to a
    /// deeper branch with strictly less cumulative support is rejected
    /// (the "deeper-fork-with-less-support" case).
    ///
    /// DESIGN: rippled's `getPreferred(largestIssued)` additionally
    /// subtracts uncommitted support (validators known to the trusted
    /// set whose latest validation is at a seq <= largestIssued and
    /// therefore "in flight"). rxrpl applies that filter at the layer
    /// above — see [`crate::validations_trie::ValidationsTrie::get_preferred`]
    /// — by rebuilding a transient trie from only the fresh subset of
    /// `latest` entries. Keeping the trie itself content-addressed by
    /// hash (no embedded seq metadata) means this method stays a pure
    /// O(D) walk and the seq-aware policy lives in one place.
    pub fn get_preferred(&self) -> Option<Hash256> {
        if self.is_empty() {
            return None;
        }
        let mut curr: &Node = &self.root;
        let mut preferred: Option<Hash256> = None;

        loop {
            // Pick the child with the highest branch_support; tie-break
            // on higher hash for determinism (matches rippled).
            let mut best: Option<&Node> = None;
            for child in curr.children.values() {
                best = Some(match best {
                    None => child,
                    Some(b) => {
                        if child.branch_support > b.branch_support
                            || (child.branch_support == b.branch_support && child.hash > b.hash)
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
    fn equal_support_fork_breaks_tie_by_higher_hash() {
        // Two branches diverging at depth 1: [1,2] and [1,3], each with
        // support 1. Tie-break = higher hash (rippled-compatible),
        // so h(3) wins over h(2).
        let mut trie = LedgerTrie::new();
        trie.insert(&[h(1), h(2)], 1);
        trie.insert(&[h(1), h(3)], 1);

        assert_eq!(trie.branch_support(&h(1)), 2);
        assert_eq!(trie.branch_support(&h(2)), 1);
        assert_eq!(trie.branch_support(&h(3)), 1);
        assert_eq!(trie.get_preferred(), Some(h(3)));
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

    // -----------------------------------------------------------------
    // T23: scenarios ported from rippled's LedgerTrie_test.cpp.
    //
    // Each rippled test addresses a `Ledger` by string prefix (`h["abc"]`)
    // which implicitly carries the ancestry "a" -> "ab" -> "abc". Our API
    // takes the explicit branch, so we use `prefix_branch("abc")` =
    // `[H("a"), H("ab"), H("abc")]`. The per-prefix hash is derived from
    // length+bytes so that distinct prefixes get distinct hashes and shared
    // prefixes get identical ones (mirroring rippled's `LedgerHistoryHelper`).
    //
    // DESIGN: rippled's `getPreferred(largestSeq)` subtracts uncommitted
    // support based on a sequence cursor. rxrpl applies that filter at the
    // ValidationsTrie layer (see `validations_trie.rs::get_preferred` and
    // its `get_preferred_drops_validations_below_current_seq` test), so
    // the rippled scenarios that exercise the subtraction directly
    // ("Too much uncommitted support", "Changing largestSeq perspective",
    // "Genesis support is NOT empty" with Seq{0}) live as integration
    // tests against the aggregator (`tests/ledger_trie_seq.rs`) rather
    // than as unit tests on the bare trie. The structural insert/remove/
    // preferred scenarios below cover everything that is intrinsic to
    // the trie's hash-only API.
    //
    // Tie-breaking: this port matches rippled's LARGER `span.startID()`
    // tie-break direction (see `LedgerTrie.h`).
    // -----------------------------------------------------------------

    /// Build the branch for a string prefix. `prefix_branch("abc")` returns
    /// `[H("a"), H("ab"), H("abc")]` where each hash is unique per prefix.
    fn prefix_branch(prefix: &str) -> Vec<Hash256> {
        let bytes = prefix.as_bytes();
        (1..=bytes.len()).map(|i| ph(&bytes[..i])).collect()
    }

    /// Hash of a prefix: byte[0]=length, byte[1..1+len]=prefix bytes.
    /// Distinct prefixes (including different lengths of the same string)
    /// always produce distinct hashes; equal prefixes produce equal hashes.
    fn ph(prefix: &[u8]) -> Hash256 {
        let mut bytes = [0u8; 32];
        // length goes first to make sure "a" and "ab" differ even if the
        // bytes-as-suffix would otherwise collide.
        bytes[0] = prefix.len() as u8;
        let copy_len = prefix.len().min(31);
        bytes[1..1 + copy_len].copy_from_slice(&prefix[..copy_len]);
        Hash256::new(bytes)
    }

    /// Convenience: tip support of the leaf identified by a string prefix.
    fn tip(trie: &LedgerTrie, prefix: &str) -> u32 {
        trie.tip_support(&ph(prefix.as_bytes()))
    }

    /// Convenience: branch support of the node identified by a string prefix.
    fn branch(trie: &LedgerTrie, prefix: &str) -> u32 {
        trie.branch_support(&ph(prefix.as_bytes()))
    }

    // ---- testInsert: "Single entry by itself" ----

    #[test]
    fn insert_same_leaf_twice_increments_tip_and_branch_support() {
        let mut trie = LedgerTrie::new();
        let abc = prefix_branch("abc");

        trie.insert(&abc, 1);
        assert_eq!(tip(&trie, "abc"), 1);
        assert_eq!(branch(&trie, "abc"), 1);

        trie.insert(&abc, 1);
        assert_eq!(tip(&trie, "abc"), 2);
        assert_eq!(branch(&trie, "abc"), 2);
    }

    // ---- testInsert: "Suffix of existing (extending tree)" ----

    #[test]
    fn insert_extending_existing_leaf_with_no_siblings() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 1);
        trie.insert(&prefix_branch("abcd"), 1);

        // abc remains a tip (still validated) and now has a descendant.
        assert_eq!(tip(&trie, "abc"), 1);
        assert_eq!(branch(&trie, "abc"), 2);
        assert_eq!(tip(&trie, "abcd"), 1);
        assert_eq!(branch(&trie, "abcd"), 1);
    }

    #[test]
    fn insert_extending_existing_leaf_with_existing_sibling() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 1);
        trie.insert(&prefix_branch("abcd"), 1);
        trie.insert(&prefix_branch("abce"), 1);

        assert_eq!(tip(&trie, "abc"), 1);
        assert_eq!(branch(&trie, "abc"), 3);
        assert_eq!(tip(&trie, "abcd"), 1);
        assert_eq!(branch(&trie, "abcd"), 1);
        assert_eq!(tip(&trie, "abce"), 1);
        assert_eq!(branch(&trie, "abce"), 1);
    }

    // ---- testInsert: "uncommitted of existing node" (insert ancestor
    // after descendants — the trie's existing internal node gains tip
    // support without disturbing children) ----

    #[test]
    fn insert_ancestor_after_descendants_credits_tip_support_only_to_ancestor() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abcd"), 1);
        trie.insert(&prefix_branch("abcdf"), 1);
        // abcd is now an internal node with tip_support=1, branch_support=2.
        assert_eq!(tip(&trie, "abcd"), 1);
        assert_eq!(branch(&trie, "abcd"), 2);

        // Insert the shorter branch — this should add tip_support to the
        // existing node "abc" without creating a new subtree.
        trie.insert(&prefix_branch("abc"), 1);

        assert_eq!(tip(&trie, "abc"), 1);
        assert_eq!(branch(&trie, "abc"), 3);
        assert_eq!(tip(&trie, "abcd"), 1);
        assert_eq!(branch(&trie, "abcd"), 2);
        assert_eq!(tip(&trie, "abcdf"), 1);
        assert_eq!(branch(&trie, "abcdf"), 1);
    }

    // ---- testInsert: "Suffix + uncommitted of existing node" — internal
    // ancestor exists implicitly with tip_support=0 once two siblings are
    // inserted. ----

    #[test]
    fn two_siblings_create_internal_ancestor_with_zero_tip_support() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abcd"), 1);
        trie.insert(&prefix_branch("abce"), 1);

        // "abc" is an implicit common ancestor: nobody validated it
        // directly, so tip_support=0, but branch_support=2.
        assert_eq!(tip(&trie, "abc"), 0);
        assert_eq!(branch(&trie, "abc"), 2);
        assert_eq!(tip(&trie, "abcd"), 1);
        assert_eq!(branch(&trie, "abcd"), 1);
        assert_eq!(tip(&trie, "abce"), 1);
        assert_eq!(branch(&trie, "abce"), 1);
    }

    #[test]
    fn three_branches_with_shared_grandparent_aggregate_branch_support() {
        // Mirrors rippled "Suffix + uncommitted with existing child".
        // abcd : abcde, abcf
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abcd"), 1);
        trie.insert(&prefix_branch("abcde"), 1);
        trie.insert(&prefix_branch("abcf"), 1);

        assert_eq!(tip(&trie, "abc"), 0);
        assert_eq!(branch(&trie, "abc"), 3);
        assert_eq!(tip(&trie, "abcd"), 1);
        assert_eq!(branch(&trie, "abcd"), 2);
        assert_eq!(tip(&trie, "abcf"), 1);
        assert_eq!(branch(&trie, "abcf"), 1);
        assert_eq!(tip(&trie, "abcde"), 1);
        assert_eq!(branch(&trie, "abcde"), 1);
    }

    // ---- testInsert: "Multiple counts" ----

    #[test]
    fn insert_with_explicit_count_credits_count_at_every_ancestor() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("ab"), 4);

        assert_eq!(tip(&trie, "ab"), 4);
        assert_eq!(branch(&trie, "ab"), 4);
        assert_eq!(tip(&trie, "a"), 0);
        assert_eq!(branch(&trie, "a"), 4);

        trie.insert(&prefix_branch("abc"), 2);
        assert_eq!(tip(&trie, "abc"), 2);
        assert_eq!(branch(&trie, "abc"), 2);
        assert_eq!(tip(&trie, "ab"), 4);
        assert_eq!(branch(&trie, "ab"), 6);
        assert_eq!(tip(&trie, "a"), 0);
        assert_eq!(branch(&trie, "a"), 6);
    }

    // ---- testRemove: "In trie but with 0 tip support" — removing an
    // implicit internal ancestor must fail without mutating support. ----

    #[test]
    fn remove_internal_ancestor_with_zero_tip_support_returns_false() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abcd"), 1);
        trie.insert(&prefix_branch("abce"), 1);
        assert_eq!(tip(&trie, "abc"), 0);
        assert_eq!(branch(&trie, "abc"), 2);

        assert!(!trie.remove(&prefix_branch("abc"), 1));
        assert_eq!(tip(&trie, "abc"), 0);
        assert_eq!(branch(&trie, "abc"), 2);
        // Children untouched.
        assert_eq!(tip(&trie, "abcd"), 1);
        assert_eq!(tip(&trie, "abce"), 1);
    }

    // ---- testRemove: "In trie with > 1 tip support" — three explicit
    // sub-cases (default-1 remove, count remove, over-remove clamp). ----

    #[test]
    fn remove_with_count_one_decrements_tip_by_one() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 2);
        assert_eq!(tip(&trie, "abc"), 2);

        assert!(trie.remove(&prefix_branch("abc"), 1));
        assert_eq!(tip(&trie, "abc"), 1);
    }

    #[test]
    fn remove_with_explicit_count_decrements_tip_by_count() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 2);
        assert!(trie.remove(&prefix_branch("abc"), 2));
        assert_eq!(tip(&trie, "abc"), 0);
        assert!(trie.is_empty());
    }

    #[test]
    fn remove_with_count_exceeding_tip_clamps_to_zero() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 3);
        assert!(trie.remove(&prefix_branch("abc"), 300));
        assert_eq!(tip(&trie, "abc"), 0);
        assert_eq!(branch(&trie, "abc"), 0);
        assert!(trie.is_empty());
    }

    // ---- testRemove: "= 1 tip support, no children" — leaf prunes
    // cleanly; parent loses one branch_support, keeps its own tip. ----

    #[test]
    fn remove_leaf_with_no_children_prunes_node_and_decrements_parent() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("ab"), 1);
        trie.insert(&prefix_branch("abc"), 1);

        assert_eq!(tip(&trie, "ab"), 1);
        assert_eq!(branch(&trie, "ab"), 2);
        assert_eq!(tip(&trie, "abc"), 1);
        assert_eq!(branch(&trie, "abc"), 1);

        assert!(trie.remove(&prefix_branch("abc"), 1));
        assert_eq!(tip(&trie, "ab"), 1);
        assert_eq!(branch(&trie, "ab"), 1);
        assert_eq!(tip(&trie, "abc"), 0);
        assert_eq!(branch(&trie, "abc"), 0);
    }

    // ---- testRemove: "= 1 tip support, 1 child" — internal node loses
    // its only tip support but retains the descendant subtree. ----

    #[test]
    fn remove_internal_with_one_child_keeps_node_in_tree() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("ab"), 1);
        trie.insert(&prefix_branch("abc"), 1);
        trie.insert(&prefix_branch("abcd"), 1);

        assert_eq!(tip(&trie, "abc"), 1);
        assert_eq!(branch(&trie, "abc"), 2);
        assert_eq!(tip(&trie, "abcd"), 1);
        assert_eq!(branch(&trie, "abcd"), 1);

        assert!(trie.remove(&prefix_branch("abc"), 1));
        assert_eq!(tip(&trie, "abc"), 0);
        // Branch support drops by 1 (the removed tip) but the child
        // subtree keeps it from going to zero.
        assert_eq!(branch(&trie, "abc"), 1);
        assert_eq!(tip(&trie, "abcd"), 1);
        assert_eq!(branch(&trie, "abcd"), 1);
    }

    // ---- testRemove: "= 1 tip support, > 1 children" — internal node
    // loses its tip but keeps both children. ----

    #[test]
    fn remove_internal_with_multiple_children_keeps_node_and_subtree() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("ab"), 1);
        trie.insert(&prefix_branch("abc"), 1);
        trie.insert(&prefix_branch("abcd"), 1);
        trie.insert(&prefix_branch("abce"), 1);

        assert_eq!(tip(&trie, "abc"), 1);
        assert_eq!(branch(&trie, "abc"), 3);

        assert!(trie.remove(&prefix_branch("abc"), 1));
        assert_eq!(tip(&trie, "abc"), 0);
        assert_eq!(branch(&trie, "abc"), 2);
        // Children still present.
        assert_eq!(tip(&trie, "abcd"), 1);
        assert_eq!(tip(&trie, "abce"), 1);
    }

    // ---- testSupport: queries on hashes never inserted return 0. ----

    #[test]
    fn support_queries_for_unknown_hashes_return_zero() {
        let trie = LedgerTrie::new();
        assert_eq!(tip(&trie, "a"), 0);
        assert_eq!(tip(&trie, "axy"), 0);
        assert_eq!(branch(&trie, "a"), 0);
        assert_eq!(branch(&trie, "axy"), 0);
    }

    // ---- testSupport: insert / sibling / remove tracks both supports. ----

    #[test]
    fn branch_and_tip_support_track_inserts_and_removes() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 1);

        assert_eq!(tip(&trie, "a"), 0);
        assert_eq!(tip(&trie, "ab"), 0);
        assert_eq!(tip(&trie, "abc"), 1);
        assert_eq!(tip(&trie, "abcd"), 0);

        assert_eq!(branch(&trie, "a"), 1);
        assert_eq!(branch(&trie, "ab"), 1);
        assert_eq!(branch(&trie, "abc"), 1);
        assert_eq!(branch(&trie, "abcd"), 0);

        trie.insert(&prefix_branch("abe"), 1);
        assert_eq!(tip(&trie, "abc"), 1);
        assert_eq!(tip(&trie, "abe"), 1);
        assert_eq!(branch(&trie, "a"), 2);
        assert_eq!(branch(&trie, "ab"), 2);

        trie.remove(&prefix_branch("abc"), 1);
        assert_eq!(tip(&trie, "abc"), 0);
        assert_eq!(tip(&trie, "abe"), 1);
        assert_eq!(branch(&trie, "a"), 1);
        assert_eq!(branch(&trie, "ab"), 1);
        assert_eq!(branch(&trie, "abc"), 0);
        assert_eq!(branch(&trie, "abe"), 1);
    }

    // ---- testGetPreferred: "Single node smaller child support" — a tip
    // with as much support as the deeper branch keeps the parent. ----

    #[test]
    fn get_preferred_stays_at_parent_when_child_has_equal_or_lesser_support() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 1);
        trie.insert(&prefix_branch("abcd"), 1);
        // tip(abc)=1, child branch_support(abcd)=1 — descend rule requires
        // strict greater, so we stop at abc.
        assert_eq!(trie.get_preferred(), Some(ph(b"abc")));
    }

    // ---- testGetPreferred: "Single node larger child" — child outweighs
    // the parent's tip and the walk descends. ----

    #[test]
    fn get_preferred_descends_when_child_branch_support_strictly_greater() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 1);
        trie.insert(&prefix_branch("abcd"), 2);
        assert_eq!(trie.get_preferred(), Some(ph(b"abcd")));
    }

    // ---- testGetPreferred: "Single node larger grand child" — grandchild
    // accumulates enough cumulative support to pull the walk all the way
    // through both parents. ----

    #[test]
    fn get_preferred_descends_through_intermediate_to_largest_grandchild() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 1);
        trie.insert(&prefix_branch("abcd"), 2);
        trie.insert(&prefix_branch("abcde"), 4);
        // branch_support: abc=7, abcd=6, abcde=4. tip_support: abc=1,
        // abcd=2. At abc, best child branch_support=6 > tip 1 -> descend.
        // At abcd, best child branch_support=4 > tip 2 -> descend to abcde.
        assert_eq!(trie.get_preferred(), Some(ph(b"abcde")));
    }

    // ---- testGetPreferred: "Single node smaller children support" —
    // three-way fork at the same depth where neither child wins outright. ----

    #[test]
    fn get_preferred_stops_when_no_child_strictly_beats_parent_tip() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 1);
        trie.insert(&prefix_branch("abcd"), 1);
        trie.insert(&prefix_branch("abce"), 1);
        // tip(abc)=1, best child branch_support=1 — not strictly greater.
        assert_eq!(trie.get_preferred(), Some(ph(b"abc")));

        // Doubling abc's tip leaves the answer unchanged.
        trie.insert(&prefix_branch("abc"), 1);
        assert_eq!(trie.get_preferred(), Some(ph(b"abc")));
    }

    // ---- testGetPreferred: "Single node larger children" — adding
    // support to a single child until it overtakes the parent's tip
    // support flips the preferred result. ----

    #[test]
    fn get_preferred_flips_to_child_after_extra_support_makes_it_strictly_greater() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 1);
        trie.insert(&prefix_branch("abcd"), 2);
        trie.insert(&prefix_branch("abce"), 1);
        // tip(abc)=1, best child branch_support(abcd)=2 > 1 -> descend.
        assert_eq!(trie.get_preferred(), Some(ph(b"abcd")));

        // Adding one more abcd vote keeps abcd preferred.
        trie.insert(&prefix_branch("abcd"), 1);
        assert_eq!(trie.get_preferred(), Some(ph(b"abcd")));
    }

    // ---- Insertion-order independence: two equivalent insertion scripts
    // produce equivalent trie state and equivalent preferred output. ----

    #[test]
    fn insertion_order_does_not_change_supports_or_preferred() {
        let mut a = LedgerTrie::new();
        a.insert(&prefix_branch("abcd"), 1);
        a.insert(&prefix_branch("abce"), 1);
        a.insert(&prefix_branch("abc"), 1);

        let mut b = LedgerTrie::new();
        b.insert(&prefix_branch("abc"), 1);
        b.insert(&prefix_branch("abcd"), 1);
        b.insert(&prefix_branch("abce"), 1);

        for prefix in ["a", "ab", "abc", "abcd", "abce"] {
            assert_eq!(tip(&a, prefix), tip(&b, prefix), "tip mismatch at {prefix}");
            assert_eq!(
                branch(&a, prefix),
                branch(&b, prefix),
                "branch mismatch at {prefix}"
            );
        }
        assert_eq!(a.get_preferred(), b.get_preferred());
    }

    // ---- Remove-then-reinsert restores prior state. ----

    #[test]
    fn remove_then_reinsert_restores_supports_and_preferred() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("abc"), 2);
        trie.insert(&prefix_branch("abcd"), 1);

        let preferred_before = trie.get_preferred();
        let tip_before = tip(&trie, "abc");
        let branch_before = branch(&trie, "abc");

        assert!(trie.remove(&prefix_branch("abc"), 1));
        trie.insert(&prefix_branch("abc"), 1);

        assert_eq!(tip(&trie, "abc"), tip_before);
        assert_eq!(branch(&trie, "abc"), branch_before);
        assert_eq!(trie.get_preferred(), preferred_before);
    }

    // ---- Multiple-removes coalesce: incremental remove(count=1) calls
    // produce the same end state as a single bulk remove. ----

    #[test]
    fn incremental_removes_coalesce_to_single_bulk_remove() {
        let mut a = LedgerTrie::new();
        a.insert(&prefix_branch("abc"), 5);
        for _ in 0..3 {
            assert!(a.remove(&prefix_branch("abc"), 1));
        }

        let mut b = LedgerTrie::new();
        b.insert(&prefix_branch("abc"), 5);
        assert!(b.remove(&prefix_branch("abc"), 3));

        for prefix in ["a", "ab", "abc"] {
            assert_eq!(tip(&a, prefix), tip(&b, prefix));
            assert_eq!(branch(&a, prefix), branch(&b, prefix));
        }
        assert_eq!(a.get_preferred(), b.get_preferred());
    }

    // ---- Branch reorganisation: the preferred answer flips when support
    // moves from one branch to another. ----

    #[test]
    fn preferred_branch_flips_when_support_moves_across_fork() {
        let mut trie = LedgerTrie::new();
        // Initial: [a, b, c] gets 3 votes, [a, b, d] gets 1 vote.
        for _ in 0..3 {
            trie.insert(&[h(0xAA), h(0xBB), h(0xCC)], 1);
        }
        trie.insert(&[h(0xAA), h(0xBB), h(0xDD)], 1);
        assert_eq!(trie.get_preferred(), Some(h(0xCC)));

        // Move 2 votes off [a,b,c] and onto [a,b,d] -> support 1 vs 3.
        assert!(trie.remove(&[h(0xAA), h(0xBB), h(0xCC)], 2));
        for _ in 0..2 {
            trie.insert(&[h(0xAA), h(0xBB), h(0xDD)], 1);
        }
        assert_eq!(trie.get_preferred(), Some(h(0xDD)));
    }

    // ---- Deep trie with mid-branch fork: depth > 20 with a sibling at
    // depth 25 — ensures recursion and traversal handle deep paths. ----

    #[test]
    fn deep_trie_with_fork_at_depth_25_returns_correct_preferred() {
        let mut trie = LedgerTrie::new();
        let main: Vec<Hash256> = (1u8..=30).map(h).collect();
        let mut fork: Vec<Hash256> = (1u8..=24).map(h).collect();
        fork.push(h(200)); // diverges at depth 25

        // Main branch has 3 votes; fork has 1 -> main wins all the way to 30.
        for _ in 0..3 {
            trie.insert(&main, 1);
        }
        trie.insert(&fork, 1);

        assert_eq!(trie.get_preferred(), Some(h(30)));
        assert_eq!(trie.branch_support(&h(1)), 4);
        assert_eq!(trie.branch_support(&h(24)), 4);
        assert_eq!(trie.branch_support(&h(25)), 3);
        assert_eq!(trie.branch_support(&h(200)), 1);

        // Remove all main-branch support; the fork becomes preferred and
        // the trie now stops at depth 25 (the fork tip).
        assert!(trie.remove(&main, 3));
        assert_eq!(trie.get_preferred(), Some(h(200)));
        assert_eq!(trie.branch_support(&h(25)), 0);
        assert_eq!(trie.branch_support(&h(200)), 1);
    }

    // ---- Pruning cascades up: removing the only descendant restores
    // the parent's branch support to just its own tip support. ----

    #[test]
    fn removing_descendant_subtree_restores_ancestor_branch_support() {
        let mut trie = LedgerTrie::new();
        trie.insert(&prefix_branch("ab"), 1);
        trie.insert(&prefix_branch("abc"), 1);
        trie.insert(&prefix_branch("abcd"), 1);

        assert_eq!(branch(&trie, "ab"), 3);

        // Remove the deepest tip first (abcd is a leaf with tip=1).
        assert!(trie.remove(&prefix_branch("abcd"), 1));
        assert_eq!(branch(&trie, "ab"), 2);
        assert_eq!(tip(&trie, "abcd"), 0);
        assert_eq!(branch(&trie, "abcd"), 0);

        // Then remove abc (now a tip with no children).
        assert!(trie.remove(&prefix_branch("abc"), 1));
        assert_eq!(branch(&trie, "ab"), 1);
        assert_eq!(tip(&trie, "ab"), 1);

        // Finally remove ab — trie empties.
        assert!(trie.remove(&prefix_branch("ab"), 1));
        assert!(trie.is_empty());
    }

    // -----------------------------------------------------------------
    // T35: explicit coverage of the tie-break direction and structural
    // edge cases that surface from the design review. The seq-aware
    // tests live alongside ValidationsTrie in
    // `crates/consensus/tests/ledger_trie_seq.rs` because the seq cursor
    // is applied at that layer, not on the bare trie.
    // -----------------------------------------------------------------

    /// Identical-seq tie at a deep fork must be broken by the HIGHER hash.
    /// Mirrors rippled's `LedgerTrie.h` `preferredChild` which compares
    /// `span.startID()` and prefers the larger one. The audit fix H4
    /// flipped our direction to match; this test pins the contract.
    #[test]
    fn equal_branch_support_at_deep_fork_breaks_to_higher_hash() {
        let mut trie = LedgerTrie::new();
        // Two siblings under a shared 4-deep prefix, both with support 2.
        trie.insert(&[h(1), h(2), h(3), h(4), h(5)], 2);
        trie.insert(&[h(1), h(2), h(3), h(4), h(9)], 2);

        // Branch supports equal at depth 5; tie-break picks h(9) > h(5).
        assert_eq!(trie.branch_support(&h(5)), 2);
        assert_eq!(trie.branch_support(&h(9)), 2);
        assert_eq!(trie.get_preferred(), Some(h(9)));
    }

    /// Tie at the FIRST level (children of the sentinel root) must also
    /// resolve to the higher hash. Edge case: the root has tip_support 0,
    /// so the early-stop rule never fires here — the comparator must do
    /// all the work. Surfaced by reading `get_preferred` and noticing the
    /// sentinel's tip_support of 0 makes level-0 a clean test for the
    /// raw comparator.
    #[test]
    fn equal_support_tie_at_root_resolves_to_higher_anchor_hash() {
        let mut trie = LedgerTrie::new();
        // Two anchors directly under root, both with support 1 at the tip.
        trie.insert(&[h(0x10)], 1);
        trie.insert(&[h(0xF0)], 1);

        assert_eq!(trie.branch_support(&h(0x10)), 1);
        assert_eq!(trie.branch_support(&h(0xF0)), 1);
        // Higher anchor hash wins.
        assert_eq!(trie.get_preferred(), Some(h(0xF0)));
    }

    /// Stress: 35 sibling branches at depth 1 (one per validator on a
    /// max-sized rxrpl UNL). Confirms the per-hash trie scales fine at
    /// the worst-case width the DESIGN justification commits to. Each
    /// branch has support 1 except one which has support 2 — that one
    /// must win, and no tie-break ambiguity should arise.
    #[test]
    fn unl_sized_fan_out_picks_unique_majority_branch() {
        const UNL_SIZE: u8 = 35;
        let mut trie = LedgerTrie::new();
        // 35 sibling tips at depth 1 from a shared anchor h(1).
        for tip_id in 2u8..2 + UNL_SIZE {
            trie.insert(&[h(1), h(tip_id)], 1);
        }
        // Boost one specific tip in the middle to support 2.
        let winner = h(20);
        trie.insert(&[h(1), winner], 1);

        assert_eq!(trie.branch_support(&h(1)), (UNL_SIZE as u32) + 1);
        assert_eq!(trie.branch_support(&winner), 2);
        assert_eq!(trie.get_preferred(), Some(winner));
    }
}
