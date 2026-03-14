use rxrpl_primitives::Hash256;

/// A batch of SHAMap nodes to store.
#[derive(Debug, Default)]
pub struct NodeBatch {
    entries: Vec<(Hash256, Vec<u8>)>,
}

impl NodeBatch {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            entries: Vec::with_capacity(cap),
        }
    }

    pub fn add(&mut self, hash: Hash256, data: Vec<u8>) {
        self.entries.push((hash, data));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Hash256, &[u8])> {
        self.entries.iter().map(|(h, d)| (h, d.as_slice()))
    }

    pub fn into_inner(self) -> Vec<(Hash256, Vec<u8>)> {
        self.entries
    }
}
