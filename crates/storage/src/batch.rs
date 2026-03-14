/// A batch of write operations to be applied atomically.
#[derive(Debug, Default)]
pub struct WriteBatch {
    ops: Vec<BatchOp>,
}

#[derive(Debug)]
enum BatchOp {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

impl WriteBatch {
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            ops: Vec::with_capacity(cap),
        }
    }

    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.ops.push(BatchOp::Put { key, value });
    }

    pub fn delete(&mut self, key: Vec<u8>) {
        self.ops.push(BatchOp::Delete { key });
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn clear(&mut self) {
        self.ops.clear();
    }

    /// Iterate over operations in this batch.
    pub fn iter(&self) -> impl Iterator<Item = BatchEntry<'_>> {
        self.ops.iter().map(|op| match op {
            BatchOp::Put { key, value } => BatchEntry::Put { key, value },
            BatchOp::Delete { key } => BatchEntry::Delete { key },
        })
    }
}

/// A reference to a single operation in a batch.
#[derive(Debug)]
pub enum BatchEntry<'a> {
    Put { key: &'a [u8], value: &'a [u8] },
    Delete { key: &'a [u8] },
}
