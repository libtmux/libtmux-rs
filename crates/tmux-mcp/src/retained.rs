//! Bounded byte storage shared by foreground and retained command readers.

/// The most command output one operation retains.
///
/// Older bytes are dropped first: the end of a command's output is what says
/// how it went.
pub(crate) const MAX_BYTES: usize = 256 * 1024;

/// How much dead prefix a command buffer holds before moving its live bytes.
pub(crate) const COMPACT_AFTER: usize = 64 * 1024;

/// A contiguous retained window with a logical front.
#[derive(Debug, Default)]
pub(crate) struct RetainedBytes {
    bytes: Vec<u8>,
    head: usize,
}

impl RetainedBytes {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn from_slice(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            head: 0,
        }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes[self.head..]
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes.len() - self.head
    }

    pub(crate) fn append(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    pub(crate) fn discard(&mut self, bytes: usize) {
        debug_assert!(bytes <= self.len());
        self.head += bytes.min(self.len());
        self.settle();
    }

    pub(crate) fn settle(&mut self) {
        if self.head >= COMPACT_AFTER {
            let retained = self.len();
            self.bytes.copy_within(self.head.., 0);
            self.bytes.truncate(retained);
            self.head = 0;
        }
        if self.bytes.capacity() > MAX_BYTES + COMPACT_AFTER {
            let retained = self.as_slice();
            debug_assert!(retained.len() <= MAX_BYTES);
            let mut bounded = Vec::with_capacity(retained.len() + COMPACT_AFTER);
            bounded.extend_from_slice(retained);
            self.bytes = bounded;
            self.head = 0;
        }
    }

    #[cfg(test)]
    pub(crate) fn physical_len(&self) -> usize {
        self.bytes.len()
    }

    #[cfg(test)]
    pub(crate) fn physical_capacity(&self) -> usize {
        self.bytes.capacity()
    }
}
