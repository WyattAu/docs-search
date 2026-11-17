//! Wire format: header, cursor, and section layout.
//!
//! Layout (all integers little-endian; format 2):
//!
//! ```text
//! magic   6 bytes  "DSIDX2"
//! version u32      format version (currently 2)
//! docs    u64      document count
//! fst_len u64      byte length of the FST dictionary section
//! vec_len u64      byte length of the quantized-vector section (0 when absent)
//! dim     u32      vector dimension (0 when absent)
//! doc_lens(docs * u64)            document byte-lengths, by doc id
//! fst(fst_len)                    term -> postings offset (u64)
//! postings  (to end - vec_len)    repeated [u32 byte_len][ (doc u32, tf u16) * ]
//! vectors(vec_len)                [u32 count][ (doc u32, scale f32, norm f32, dim * i8) * ]
//! ```
//!
//! Format 1 carried presence bitmaps and unit term frequency; format 2
//! replaced the bitmaps with explicit (doc, tf) pair lists so ranking has
//! a real frequency signal, and added the fixed-header vector fields the
//! `vectors` feature reads and writes.

use crate::error::IndexError;

/// The 6-byte magic every index starts with.
pub(crate) const MAGIC: [u8; 6] = *b"DSIDX2";

/// The format version this crate writes and decodes.
pub(crate) const VERSION: u32 = 2;

/// Bounds-checked little-endian reader over a byte slice.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// A reader positioned at the start of `buf`.
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes consumed so far.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// Advance past `n` bytes (used to jump the fixed header).
    pub(crate) fn skip(&mut self, n: usize) -> Result<(), IndexError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(IndexError::Truncated { section: "header" })?;
        if end > self.buf.len() {
            return Err(IndexError::Truncated { section: "header" });
        }
        self.pos = end;
        Ok(())
    }

    /// The remaining, unconsumed bytes.
    pub(crate) fn rest(&self) -> &'a [u8] {
        self.buf.get(self.pos..).unwrap_or(&[])
    }

    /// Read one `u64`, advancing the cursor.
    pub(crate) fn u64(&mut self) -> Result<u64, IndexError> {
        let bytes = self.take(8).ok_or(IndexError::Truncated {
            section: "u64 field",
        })?;
        let mut raw = [0u8; 8];
        raw.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(raw))
    }

    /// Read one `u32`, advancing the cursor.
    pub(crate) fn u32(&mut self) -> Result<u32, IndexError> {
        let bytes = self.take(4).ok_or(IndexError::Truncated {
            section: "u32 field",
        })?;
        let mut raw = [0u8; 4];
        raw.copy_from_slice(bytes);
        Ok(u32::from_le_bytes(raw))
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let bytes = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(bytes)
    }

    /// Read one `f32`, advancing the cursor. Used by the vectors
    /// feature's quantized-vector section (0.3.0+).
    pub(crate) fn f32(&mut self) -> Result<f32, IndexError> {
        let bytes = self.take(4).ok_or(IndexError::Truncated {
            section: "f32 field",
        })?;
        let mut raw = [0u8; 4];
        raw.copy_from_slice(bytes);
        Ok(f32::from_le_bytes(raw))
    }

    /// Read one `i8`, advancing the cursor. Used by the vectors
    /// feature's quantized-vector section (0.3.0+).
    pub(crate) fn i8(&mut self) -> Result<i8, IndexError> {
        let bytes = self.take(1).ok_or(IndexError::Truncated {
            section: "i8 field",
        })?;
        bytes
            .first()
            .copied()
            .map(|b| b as i8)
            .ok_or(IndexError::Truncated {
                section: "i8 field",
            })
    }
}

/// Append a little-endian `u64` to a buffer.
pub(crate) fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Append a little-endian `u32` to a buffer.
pub(crate) fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Validate the 10-byte header (magic + version) of an index blob.
pub(crate) fn check_header(bytes: &[u8]) -> Result<(), IndexError> {
    let magic = bytes
        .get(0..6)
        .ok_or(IndexError::Truncated { section: "magic" })?;
    if magic != MAGIC {
        let mut found = [0u8; 6];
        for (slot, byte) in found.iter_mut().zip(magic) {
            *slot = *byte;
        }
        return Err(IndexError::InvalidMagic { found });
    }
    let version = bytes
        .get(6..10)
        .map(|b| {
            let mut raw = [0u8; 4];
            raw.copy_from_slice(b);
            u32::from_le_bytes(raw)
        })
        .ok_or(IndexError::Truncated { section: "version" })?;
    if version > VERSION {
        return Err(IndexError::UnsupportedVersion {
            found: version,
            supported: VERSION,
        });
    }
    Ok(())
}
