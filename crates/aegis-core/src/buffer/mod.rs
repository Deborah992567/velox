//! Growable byte buffers with cursor-based reclamation.
//!
//! An [`IoBuf`] is the memory equivalent of nginx's `ngx_buf_t`: unread bytes
//! live between an independent read cursor (`start`) and write cursor (`end`).
//! Consumed bytes are reclaimed in bulk — the live tail is moved down to the
//! front — so a parser that repeatedly reads-then-consumes a request stream
//! never grows its allocation. Reclamation is amortized O(1): the only cost is
//! a `memmove` of the live bytes when a write would otherwise need more space.

use std::fmt;
use std::io;

/// A byte buffer with separate read and write cursors.
///
/// Invariant: `data.len() == end` at every method boundary (the spare window
/// returned by [`IoBuf::spare_mut`] temporarily extends `len` until the caller
/// reports how much was filled via [`IoBuf::advance_written`]).
pub struct IoBuf {
    data: Vec<u8>,
    start: usize,
    end: usize,
}

impl IoBuf {
    /// An empty buffer.
    pub const fn new() -> Self {
        Self {
            data: Vec::new(),
            start: 0,
            end: 0,
        }
    }

    /// An empty buffer with room for `capacity` bytes.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            data: Vec::with_capacity(capacity),
            start: 0,
            end: 0,
        }
    }

    /// Number of unread bytes.
    pub const fn len(&self) -> usize {
        self.end - self.start
    }

    /// Whether no unread bytes remain.
    pub const fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Total allocated capacity, including consumed and spare space.
    pub const fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// Number of bytes consumed so far since the last [`IoBuf::reclaim`].
    pub const fn consumed(&self) -> usize {
        self.start
    }

    /// Reset the buffer to empty, keeping the allocation.
    pub fn clear(&mut self) {
        self.data.clear();
        self.start = 0;
        self.end = 0;
    }

    /// The unread region, read-only.
    pub fn peek(&self) -> &[u8] {
        &self.data[self.start..self.end]
    }

    /// The first unread byte, if any.
    pub fn peek_byte(&self) -> Option<u8> {
        self.peek().first().copied()
    }

    /// Append `bytes` at the write cursor.
    pub fn put(&mut self, bytes: &[u8]) {
        self.reserve(bytes.len());
        self.data.extend_from_slice(bytes);
        self.end += bytes.len();
    }

    /// Copy up to `out.len()` unread bytes into `out`, consuming them.
    ///
    /// Returns the number of bytes copied.
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len());
        out[..n].copy_from_slice(&self.data[self.start..self.start + n]);
        self.start += n;
        n
    }

    /// Advance the read cursor by `n` bytes without copying.
    ///
    /// # Panics
    ///
    /// Panics if `n` exceeds [`IoBuf::len`], which would desynchronize the
    /// cursors.
    pub fn consume(&mut self, n: usize) {
        assert!(
            n <= self.len(),
            "cannot consume {n} of {} unread bytes",
            self.len()
        );
        self.start += n;
    }

    /// If the buffer starts with `prefix`, consume it and return `true`.
    pub fn try_consume(&mut self, prefix: &[u8]) -> bool {
        if self.peek().starts_with(prefix) {
            self.consume(prefix.len());
            true
        } else {
            false
        }
    }

    /// Ensure room for at least `additional` more bytes after the write
    /// cursor, reclaiming consumed space and growing only when necessary.
    pub fn reserve(&mut self, additional: usize) {
        if self.data.capacity() - self.end < additional {
            self.reclaim();
        }
        if self.data.capacity() - self.end < additional {
            self.data
                .reserve(additional - (self.data.capacity() - self.end));
        }
    }

    /// Move the live tail down to the front, freeing consumed space.
    ///
    /// Returns the number of bytes reclaimed (the previous read-cursor
    /// position). This is a no-op when nothing has been consumed.
    pub fn reclaim(&mut self) -> usize {
        if self.start == 0 {
            return 0;
        }
        let live = self.end - self.start;
        self.data.copy_within(self.start..self.end, 0);
        self.data.truncate(live);
        let reclaimed = self.start;
        self.start = 0;
        self.end = live;
        reclaimed
    }

    /// A writable window of at least `minimum` bytes at the write cursor, for
    /// reading straight off a socket. Must be followed by
    /// [`IoBuf::advance_written`] with the number of bytes actually filled.
    ///
    /// The window is zero-initialized so unread regions stay deterministic.
    pub fn spare_mut(&mut self, minimum: usize) -> &mut [u8] {
        self.reserve(minimum);
        self.data.resize(self.end + minimum, 0);
        &mut self.data[self.end..]
    }

    /// Record that `n` bytes were written into the most recent
    /// [`IoBuf::spare_mut`] window.
    ///
    /// # Panics
    ///
    /// Panics if `n` exceeds the window size returned by the preceding
    /// `spare_mut` call.
    pub fn advance_written(&mut self, n: usize) {
        assert!(
            self.end + n <= self.data.len(),
            "advance {n} beyond spare window"
        );
        self.end += n;
        self.data.truncate(self.end);
    }
}

impl Default for IoBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for IoBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IoBuf")
            .field("len", &self.len())
            .field("consumed", &self.consumed())
            .field("capacity", &self.capacity())
            .field("data", &self.peek())
            .finish()
    }
}

/// Read raw bytes from `reader` into the buffer's spare window, up to `limit`.
///
/// Returns the number of bytes appended, or `0` on EOF.
pub fn read_into(buf: &mut IoBuf, reader: &mut impl io::Read, limit: usize) -> io::Result<usize> {
    let window = buf.spare_mut(limit);
    let n = reader.read(window)?;
    buf.advance_written(n);
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::{IoBuf, read_into};
    use std::io::Cursor;

    #[test]
    fn put_read_and_len_track_cursors() {
        let mut buf = IoBuf::new();
        assert!(buf.is_empty());
        buf.put(b"hello");
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.peek(), b"hello");

        let mut out = [0u8; 3];
        assert_eq!(buf.read(&mut out), 3);
        assert_eq!(&out, b"hel");
        assert_eq!(buf.len(), 2);
        assert_eq!(buf.peek(), b"lo");
    }

    #[test]
    fn consume_advances_read_cursor() {
        let mut buf = IoBuf::new();
        buf.put(b"abcdef");
        buf.consume(2);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.peek(), b"cdef");
        assert_eq!(buf.consumed(), 2);
    }

    #[test]
    fn try_consume_matches_prefix_only() {
        let mut buf = IoBuf::new();
        buf.put(b"\r\n\r\nX");
        assert!(buf.try_consume(b"\r\n\r\n"));
        assert_eq!(buf.peek(), b"X");
        assert!(!buf.try_consume(b"\r\n"));
        assert_eq!(buf.peek(), b"X", "a non-matching prefix must not consume");
    }

    #[test]
    fn reclaim_moves_live_tail_to_front() {
        let mut buf = IoBuf::with_capacity(64);
        buf.put(b"0123456789");
        let mut out = [0u8; 4];
        buf.read(&mut out); // consume "0123"
        assert_eq!(buf.consumed(), 4);

        let reclaimed = buf.reclaim();
        assert_eq!(reclaimed, 4);
        assert_eq!(buf.consumed(), 0);
        assert_eq!(buf.len(), 6);
        assert_eq!(buf.peek(), b"456789");
        assert!(
            buf.capacity() >= 64,
            "reclaim must not shrink the allocation"
        );
    }

    #[test]
    fn repeated_reclaim_never_grows() {
        let mut buf = IoBuf::with_capacity(16);
        for _ in 0..100 {
            buf.put(b"hello world, hello world");
            let mut out = [0u8; 24];
            buf.read(&mut out);
            buf.reclaim();
        }
        assert_eq!(
            buf.capacity(),
            32,
            "parse-loop churn must not grow memory once the working set fits"
        );
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn spare_window_fills_and_truncates() {
        let mut buf = IoBuf::with_capacity(32);
        {
            let window = buf.spare_mut(16);
            assert!(
                window.len() >= 16,
                "window must be at least the requested size"
            );
            window[..5].copy_from_slice(b"hello");
        }
        buf.advance_written(5);
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.peek(), b"hello");
        assert_eq!(
            buf.capacity(),
            32,
            "a 16-byte window must fit the existing allocation"
        );
    }

    #[test]
    fn read_into_pulls_from_reader() {
        let mut buf = IoBuf::new();
        let mut src = Cursor::new(b"stream of bytes".to_vec());
        let n = read_into(&mut buf, &mut src, 6).unwrap();
        assert_eq!(n, 6);
        assert_eq!(buf.peek(), b"stream");
        let n = read_into(&mut buf, &mut src, 100).unwrap();
        assert_eq!(n, 9);
        assert_eq!(buf.peek(), b"stream of bytes");
        assert_eq!(read_into(&mut buf, &mut src, 100).unwrap(), 0, "EOF");
    }

    #[test]
    fn clear_resets_cursors() {
        let mut buf = IoBuf::with_capacity(32);
        buf.put(b"data");
        buf.read(&mut [0u8; 2]);
        buf.clear();
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.consumed(), 0);
        assert!(buf.is_empty());
    }
}
