//! Builder, reader, and query half of the index.

use std::collections::BTreeMap;

use fst::{MapBuilder, Streamer};
use roaring::RoaringBitmap;

use crate::error::IndexError;
use crate::format::{check_header, push_u32, push_u64, Reader, MAGIC, VERSION};

/// Run one dictionary operation with an unwind boundary.
///
/// fst 0.4.7 panics internally on hostile dictionaries (raw/node.rs
/// out-of-bounds and subtraction overflow; found by the corruption property
/// test). The estate decoder pattern requires this crate to be total on
/// untrusted input, so every fst entry point is guarded and an unwind
/// surfaces as a typed error. Panic = unwind in this crate's profiles;
/// do not introduce `panic = "abort"`.
fn fst_guard<T, E>(op: impl FnOnce() -> Result<T, E>) -> Result<T, IndexError>
where
    E: std::error::Error + 'static,
    IndexError: From<E>,
{
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(op))
        .map_err(|_| IndexError::Corrupt {
            reason: "term dictionary rejected hostile input",
        })?
        .map_err(IndexError::from)
}

/// BM25 k1 saturation parameter; ranking is documented as unit-tf (0.1 scope).
const K1: f64 = 1.2;
/// BM25 length-normalization parameter.
const B: f64 = 0.75;

/// Split text into index terms: lowercase, contiguous alphanumeric runs.
/// Owned terms, so builders and queries can hold them freely.
fn terms(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
}

/// Collects documents into a deterministic, serialized index.
///
/// Document ids are assigned in insertion order; serialization is
/// byte-for-byte reproducible for the same input (BTreeMap postings,
/// stable counter order).
#[derive(Debug, Default)]
pub struct IndexBuilder {
    postings: BTreeMap<String, RoaringBitmap>,
    doc_lens: Vec<u64>,
}

impl IndexBuilder {
    /// A fresh builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one document; its id is the insertion ordinal.
    pub fn add_doc(&mut self, text: &str) {
        let doc = self.doc_lens.len() as u64;
        let mut len = 0u64;
        for term in terms(text) {
            len += term.len() as u64;
            self.postings.entry(term).or_default().insert(doc as u32);
        }
        self.doc_lens.push(len);
    }

    /// Serialize the index to its binary format.
    ///
    /// The only failure modes are serialization-layer bugs (an FST key
    /// ordering violation or a bitmap encode error); they surface as typed
    /// errors, never panics.
    pub fn build(&self) -> Result<Vec<u8>, IndexError> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        push_u32(&mut out, VERSION);
        push_u64(&mut out, self.doc_lens.len() as u64);

        // Postings blob first so the FST offsets are known before the
        // dictionary is serialized.
        let mut postings = Vec::new();
        let mut entries: Vec<(&str, u64)> = Vec::with_capacity(self.postings.len());
        for (term, bitmap) in &self.postings {
            let size =
                u32::try_from(bitmap.serialized_size()).map_err(|_| IndexError::Corrupt {
                    reason: "bitmap exceeds u32 length",
                })?;
            entries.push((term, postings.len() as u64));
            push_u32(&mut postings, size);
            bitmap
                .serialize_into(&mut postings)
                .map_err(|_| IndexError::Corrupt {
                    reason: "bitmap serialization failed",
                })?;
        }

        let mut wtr = MapBuilder::new(Vec::new()).map_err(|_| IndexError::Corrupt {
            reason: "fst builder init failed",
        })?;
        for (term, offset) in entries {
            // Builder insert only fails on out-of-order keys; BTreeMap
            // iteration is byte-sorted, which is the order FST requires.
            wtr.insert(term.as_bytes(), offset)
                .map_err(|_| IndexError::Corrupt {
                    reason: "fst key ordering",
                })?;
        }
        let fst_bytes = wtr.into_inner().map_err(|_| IndexError::Corrupt {
            reason: "fst finalize failed",
        })?;

        push_u64(&mut out, fst_bytes.len() as u64);
        for len in &self.doc_lens {
            push_u64(&mut out, *len);
        }
        out.extend_from_slice(&fst_bytes);
        out.extend_from_slice(&postings);
        Ok(out)
    }
}

/// One scored query result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// Document id (insertion ordinal).
    pub doc: u32,
    /// BM25-shaped relevance score; higher is better.
    pub score: f64,
}

/// A loaded search index: queryable, wasm-clean, fully read-only.
#[derive(Debug)]
pub struct Index {
    fst_map: fst::Map<Vec<u8>>,
    postings: Vec<u8>,
    doc_lens: Vec<u64>,
}

impl Index {
    /// Decode an index blob. Total: malformed input yields a typed error,
    /// never a panic (the index is untrusted input, per the estate decoder
    /// pattern).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        check_header(bytes)?;
        let mut r = Reader::new(bytes);
        r.skip(10)?; // magic + version, already validated
        let doc_count = r.u64()?;
        let fst_len = r.u64()?;

        // A doc table entry costs at least 8 bytes; a larger count than the
        // remaining bytes can express is truncation, not an allocation bomb.
        if doc_count > r.rest().len() as u64 / 8 {
            return Err(IndexError::Truncated {
                section: "doc table",
            });
        }
        let doc_lens = read_doc_lens(&mut r, doc_count)?;

        let fst_start = r.pos();
        let fst_end = fst_start
            .checked_add(usize::try_from(fst_len).map_err(|_| IndexError::Corrupt {
                reason: "fst length exceeds address space",
            })?)
            .ok_or(IndexError::Corrupt {
                reason: "fst section overflows",
            })?;
        let fst_bytes = bytes.get(fst_start..fst_end).ok_or(IndexError::Truncated {
            section: "fst dictionary",
        })?;
        let postings = bytes.get(fst_end..).ok_or(IndexError::Truncated {
            section: "postings",
        })?;

        let fst_map = fst_guard(|| fst::Map::new(fst_bytes.to_vec()))?;
        validate_postings(postings)?;
        // Cross-section coherence: every dictionary offset must resolve to
        // a bitmap inside the postings area, so a decoded index can never
        // produce a silent empty query from an unresolvable term. Streaming
        // the whole dictionary here also exercises every node, which is
        // what makes query-time reads panic-free in practice.
        fst_guard(|| {
            let mut terms = fst_map.stream();
            while let Some((_, offset)) = terms.next() {
                read_bitmap_at(postings, offset as usize)?;
            }
            Ok::<(), IndexError>(())
        })?;
        Ok(Self {
            fst_map,
            postings: postings.to_vec(),
            doc_lens,
        })
    }

    /// Number of documents.
    pub fn len(&self) -> u64 {
        self.doc_lens.len() as u64
    }

    /// Whether there are no documents.
    pub fn is_empty(&self) -> bool {
        self.doc_lens.is_empty()
    }

    /// AND-query the index, ranked by BM25-shaped score (unit term
    /// frequency), best first, at most `limit` hits.
    ///
    /// A query term missing from the dictionary yields no hits (AND
    /// semantics). Corrupt postings for a queried term also yield no hits:
    /// query is a read path and reports emptiness rather than erroring.
    pub fn query(&self, query: &str, limit: usize) -> Vec<Hit> {
        let query_terms: Vec<String> = terms(query).collect();
        if query_terms.is_empty() {
            return Vec::new();
        }

        let mut matched: Vec<RoaringBitmap> = Vec::new();
        for term in &query_terms {
            match self.postings_for(term) {
                Ok(Some(bm)) => matched.push(bm),
                Ok(None) | Err(_) => return Vec::new(),
            }
        }
        let Some(first) = matched.first() else {
            return Vec::new();
        };
        let mut docs = first.clone();
        for bm in &matched {
            docs &= bm;
        }

        let n = self.len();
        let avgdl = average(&self.doc_lens);
        let mut hits: Vec<Hit> = Vec::new();
        for doc in docs {
            let Some(dl) = self.doc_lens.get(doc as usize) else {
                continue;
            };
            let mut score = 0.0;
            for term in &query_terms {
                if let Ok(Some(bm)) = self.postings_for(term) {
                    if bm.contains(doc) {
                        let idf = idf(n, bm.len());
                        score += idf * norm(*dl, avgdl);
                    }
                }
            }
            hits.push(Hit { doc, score });
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.doc.cmp(&b.doc)));
        hits.truncate(limit);
        hits
    }

    fn postings_for(&self, term: &str) -> Result<Option<RoaringBitmap>, IndexError> {
        // The dictionary was fully streamed at decode, so reads cannot
        // panic on a decoded index; the guard is defense-in-depth so the
        // query path stays total regardless.
        let lookup = std::panic::catch_unwind(|| self.fst_map.get(term.as_bytes()));
        let offset = match lookup {
            Ok(Some(offset)) => offset,
            Ok(None) => return Ok(None),
            Err(_) => {
                return Err(IndexError::Corrupt {
                    reason: "term dictionary rejected hostile input",
                })
            }
        };
        read_bitmap_at(&self.postings, offset as usize).map(Some)
    }
}

/// Bounds-checked bitmap read at `start`: `u32` length prefix + roaring
/// bytes. Shared by decode-time validation and query-time lookups.
fn read_bitmap_at(postings: &[u8], start: usize) -> Result<RoaringBitmap, IndexError> {
    let len_pos = start.checked_add(4).ok_or(IndexError::Corrupt {
        reason: "postings offset overflows",
    })?;
    let len_bytes = postings.get(start..len_pos).ok_or(IndexError::Corrupt {
        reason: "postings offset out of range",
    })?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(len_bytes);
    let len = usize::try_from(u32::from_le_bytes(raw)).map_err(|_| IndexError::Corrupt {
        reason: "bitmap length exceeds address space",
    })?;
    let end = len_pos.checked_add(len).ok_or(IndexError::Corrupt {
        reason: "bitmap length overflows",
    })?;
    let bytes = postings.get(len_pos..end).ok_or(IndexError::Corrupt {
        reason: "bitmap bytes out of range",
    })?;
    RoaringBitmap::deserialize_from(bytes).map_err(|_| IndexError::Corrupt {
        reason: "bitmap deserialization",
    })
}

/// Walk the entire postings area: every entry's length must fit and every
/// bitmap must deserialize, and the entries must exactly cover the area
/// (no trailing garbage, no silently-empty truncation). Decode-time
/// validation means a decoded index can always answer queries; corruption
/// is never deferred to a silent empty result later.
fn validate_postings(postings: &[u8]) -> Result<(), IndexError> {
    let mut pos = 0usize;
    while pos < postings.len() {
        let len_pos = pos.checked_add(4).ok_or(IndexError::Corrupt {
            reason: "postings offset overflows",
        })?;
        let len_bytes = postings.get(pos..len_pos).ok_or(IndexError::Corrupt {
            reason: "truncated bitmap length",
        })?;
        let mut raw = [0u8; 4];
        raw.copy_from_slice(len_bytes);
        let len = usize::try_from(u32::from_le_bytes(raw)).map_err(|_| IndexError::Corrupt {
            reason: "bitmap length exceeds address space",
        })?;
        let end = len_pos.checked_add(len).ok_or(IndexError::Corrupt {
            reason: "bitmap length overflows",
        })?;
        if end > postings.len() {
            return Err(IndexError::Corrupt {
                reason: "bitmap bytes out of range",
            });
        }
        let _ = read_bitmap_at(postings, pos)?;
        pos = end;
    }
    if pos != postings.len() {
        return Err(IndexError::Corrupt {
            reason: "postings not fully consumed",
        });
    }
    Ok(())
}

fn read_doc_lens(r: &mut Reader<'_>, doc_count: u64) -> Result<Vec<u64>, IndexError> {
    let mut lens = Vec::new();
    for _ in 0..doc_count {
        lens.push(r.u64()?);
    }
    Ok(lens)
}

fn average(doc_lens: &[u64]) -> f64 {
    if doc_lens.is_empty() {
        return 1.0;
    }
    let total: u64 = doc_lens.iter().sum();
    if total == 0 {
        1.0
    } else {
        total as f64 / doc_lens.len() as f64
    }
}

fn idf(n_docs: u64, n_matching: u64) -> f64 {
    let n = n_docs as f64;
    let matching = n_matching as f64;
    (1.0 + (n - matching + 0.5) / (matching + 0.5)).ln()
}

/// Length normalization: `(k1 + 1) / (tf + k1 * (1 - b + b * dl / avgdl))`
/// with unit term frequency (tf = 1), per the 0.1 scope note.
fn norm(dl: u64, avgdl: f64) -> f64 {
    let dl = dl as f64;
    (K1 + 1.0) / (1.0 + K1 * (1.0 - B + B * dl / avgdl))
}
