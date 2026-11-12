//! Builder, reader, and query half of the index.

use std::collections::BTreeMap;

use fst::{MapBuilder, Streamer};

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

/// BM25 saturation parameter `k1`.
const K1: f64 = 1.2;
/// BM25 length-normalization parameter `b`.
const B: f64 = 0.75;

/// Split text into index terms: lowercase, contiguous alphanumeric runs.
/// Owned terms, so builders and queries can hold them freely.
fn terms(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
}

/// One term's postings: (doc id, term frequency) pairs, doc ids strictly
/// increasing, tf at least 1. The ordering is an invariant of both the
/// builder (BTreeMap iteration) and the decoder (validation rejects
/// anything else), so queries may binary-search.
type PairList = Vec<(u32, u16)>;

/// A quantized vector: fixed max-abs i8 components with the scale needed
/// to dequantize, and the L2 norm of the quantized vector for cosine.
#[cfg(feature = "vectors")]
#[derive(Debug, Clone)]
struct QuantVec {
    /// Per-vector scale: `dim * scale` reconstructs the approximate f32.
    scale: f32,
    /// L2 norm of the quantized vector; 0.0 marks the zero vector.
    norm: f32,
    /// Quantized components, one per dimension.
    dims: Vec<i8>,
}

/// Quantize one f32 vector: max-abs scaling into [-127, 127] i8.
/// Deterministic by construction (round, clamp, integer-summable norms).
#[cfg(feature = "vectors")]
fn quantize(vector: &[f32]) -> QuantVec {
    let max_abs = vector.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let scale = if max_abs > 0.0 { 127.0 / max_abs } else { 0.0 };
    let dims: Vec<i8> = vector
        .iter()
        .map(|v| {
            let q = (v * scale).round();
            if q >= 127.0 {
                127
            } else if q <= -127.0 {
                -127
            } else {
                q as i8
            }
        })
        .collect();
    let norm = dims
        .iter()
        .map(|q| (*q as f32) * (*q as f32))
        .sum::<f32>()
        .sqrt();
    QuantVec { scale, norm, dims }
}

/// Collects documents into a deterministic, serialized index.
///
/// Document ids are assigned in insertion order; serialization is
/// byte-for-byte reproducible for the same input (BTreeMap postings,
/// stable counter order).
#[derive(Debug, Default)]
pub struct IndexBuilder {
    postings: BTreeMap<String, BTreeMap<u32, u16>>,
    doc_lens: Vec<u64>,
    /// Doc id -> quantized vector, insertion-time quantization.
    #[cfg(feature = "vectors")]
    vectors: BTreeMap<u32, QuantVec>,
}

impl IndexBuilder {
    /// A fresh builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one document; returns its id (the insertion ordinal).
    pub fn add_doc(&mut self, text: &str) -> u64 {
        let doc = self.doc_lens.len() as u64;
        let mut len = 0u64;
        for term in terms(text) {
            len += term.len() as u64;
            let counts = self.postings.entry(term).or_default();
            // Saturate rather than overflow: a document repeating a term
            // past u16::MAX has long since exhausted the ranking signal.
            let tf = counts.entry(doc as u32).or_insert(0);
            *tf = tf.saturating_add(1);
        }
        self.doc_lens.push(len);
        doc
    }

    /// Attach a quantized embedding to a document already added (`vectors`
    /// feature). The first vector fixes the index's dimension; later
    /// vectors must match it or this returns [`IndexError::VectorDim`].
    #[cfg(feature = "vectors")]
    pub fn set_vector(&mut self, doc: u32, vector: &[f32]) -> Result<(), IndexError> {
        if vector.is_empty() {
            return Err(IndexError::Corrupt {
                reason: "vector dimension must be at least 1",
            });
        }
        if usize::try_from(doc).map_or(true, |d| d >= self.doc_lens.len()) {
            return Err(IndexError::Corrupt {
                reason: "vector attached to an unknown doc id",
            });
        }
        if let Some(expected) = self.vectors.values().next().map(|q| q.dims.len() as u32) {
            let found = vector.len() as u32;
            if expected != found {
                return Err(IndexError::VectorDim { expected, found });
            }
        }
        self.vectors.insert(doc, quantize(vector));
        Ok(())
    }

    /// Serialize the index to its binary format.
    ///
    /// The only failure modes are serialization-layer bugs (an FST key
    /// ordering violation or a count that cannot be represented); they
    /// surface as typed errors, never panics.
    pub fn build(&self) -> Result<Vec<u8>, IndexError> {
        // Postings blob first so the FST offsets are known before the
        // dictionary is serialized.
        let mut postings = Vec::new();
        let mut entries: Vec<(&str, u64)> = Vec::with_capacity(self.postings.len());
        for (term, docs) in &self.postings {
            let size = u32::try_from(docs.len() * 6).map_err(|_| IndexError::Corrupt {
                reason: "postings exceed u32 length",
            })?;
            entries.push((term, postings.len() as u64));
            push_u32(&mut postings, size);
            for (doc, tf) in docs {
                push_u32(&mut postings, *doc);
                postings.extend_from_slice(&tf.to_le_bytes());
            }
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

        // The vectors section: count-prefixed entries sorted by doc id.
        #[cfg(feature = "vectors")]
        let (vec_bytes, dim): (Vec<u8>, u32) = {
            match self.vectors.values().next() {
                None => (Vec::new(), 0),
                Some(first) => {
                    let mut vb = Vec::new();
                    push_u32(&mut vb, self.vectors.len() as u32);
                    for (doc, q) in &self.vectors {
                        push_u32(&mut vb, *doc);
                        vb.extend_from_slice(&q.scale.to_le_bytes());
                        vb.extend_from_slice(&q.norm.to_le_bytes());
                        for d in &q.dims {
                            vb.push(*d as u8);
                        }
                    }
                    (vb, first.dims.len() as u32)
                }
            }
        };
        #[cfg(not(feature = "vectors"))]
        let (vec_bytes, dim): (Vec<u8>, u32) = (Vec::new(), 0);

        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        push_u32(&mut out, VERSION);
        push_u64(&mut out, self.doc_lens.len() as u64);
        push_u64(&mut out, fst_bytes.len() as u64);
        push_u64(&mut out, vec_bytes.len() as u64);
        push_u32(&mut out, dim);
        for len in &self.doc_lens {
            push_u64(&mut out, *len);
        }
        out.extend_from_slice(&fst_bytes);
        out.extend_from_slice(&postings);
        out.extend_from_slice(&vec_bytes);
        Ok(out)
    }
}

/// One scored query result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// Document id (insertion ordinal).
    pub doc: u32,
    /// Relevance score: BM25 for [`Index::query`], approximate cosine for
    /// [`Index::query_vector`]. Higher is better.
    pub score: f64,
}

/// A loaded search index: queryable, wasm-clean, fully read-only.
#[derive(Debug)]
pub struct Index {
    fst_map: fst::Map<Vec<u8>>,
    postings: Vec<u8>,
    doc_lens: Vec<u64>,
    /// Doc-id-sorted quantized vectors, decoded when the section exists.
    #[cfg(feature = "vectors")]
    vectors: Vec<(u32, QuantVec)>,
}

impl Index {
    /// Decode an index blob. Total: malformed input yields a typed error,
    /// never a panic (the index is untrusted input, per the estate decoder
    /// pattern). An index carrying vectors decodes only with the
    /// `vectors` feature compiled in.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        check_header(bytes)?;
        let mut r = Reader::new(bytes);
        r.skip(10)?; // magic + version, already validated
        let doc_count = r.u64()?;
        let fst_len = r.u64()?;
        let vec_len = r.u64()?;
        let dim = r.u32()?;

        if vec_len == 0 && dim != 0 {
            return Err(IndexError::Corrupt {
                reason: "dimension declared with no vectors section",
            });
        }
        #[cfg(not(feature = "vectors"))]
        if vec_len > 0 {
            return Err(IndexError::VectorsUnsupported);
        }

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
        let tail = bytes.get(fst_end..).ok_or(IndexError::Truncated {
            section: "postings",
        })?;
        let vec_len = usize::try_from(vec_len).map_err(|_| IndexError::Corrupt {
            reason: "vectors length exceeds address space",
        })?;
        if vec_len > tail.len() {
            return Err(IndexError::Corrupt {
                reason: "vectors section overruns the blob",
            });
        }
        let postings_end = tail.len() - vec_len;
        let postings = tail.get(0..postings_end).ok_or(IndexError::Corrupt {
            reason: "postings section bounds",
        })?;

        let fst_map = fst_guard(|| fst::Map::new(fst_bytes.to_vec()))?;
        validate_postings(postings, doc_count)?;

        // Cross-section coherence: every dictionary offset must resolve to
        // a postings list inside the area, so a decoded index can never
        // produce a silent empty query from an unresolvable term. Streaming
        // the whole dictionary here also exercises every node, which is
        // what makes query-time reads panic-free in practice.
        fst_guard(|| {
            let mut terms = fst_map.stream();
            while let Some((_, offset)) = terms.next() {
                read_pairs_at(postings, offset as usize)?;
            }
            Ok::<(), IndexError>(())
        })?;

        #[cfg(feature = "vectors")]
        let vectors = if vec_len == 0 {
            Vec::new()
        } else {
            let vec_bytes = tail.get(postings_end..).ok_or(IndexError::Corrupt {
                reason: "vectors section bounds",
            })?;
            parse_vectors(vec_bytes, dim, doc_count)?
        };
        Ok(Self {
            fst_map,
            postings: postings.to_vec(),
            doc_lens,
            #[cfg(feature = "vectors")]
            vectors,
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

    /// AND-query the index, ranked by BM25 over true term frequencies,
    /// best first, at most `limit` hits.
    ///
    /// A query term missing from the dictionary yields no hits (AND
    /// semantics). Corrupt postings for a queried term also yield no hits:
    /// query is a read path and reports emptiness rather than erroring.
    pub fn query(&self, query: &str, limit: usize) -> Vec<Hit> {
        let query_terms: Vec<String> = terms(query).collect();
        if query_terms.is_empty() {
            return Vec::new();
        }

        let mut lists: Vec<PairList> = Vec::with_capacity(query_terms.len());
        for term in &query_terms {
            match self.postings_for(term) {
                Ok(Some(list)) => lists.push(list),
                Ok(None) | Err(_) => return Vec::new(),
            }
        }
        let Some(first) = lists.first() else {
            return Vec::new();
        };

        let n = self.len();
        let avgdl = average(&self.doc_lens);
        let mut hits: Vec<Hit> = Vec::new();
        for &(doc, _) in first {
            let Some(dl) = self.doc_lens.get(doc as usize) else {
                continue;
            };
            // AND: the candidate must appear in every other list.
            let in_all = lists
                .iter()
                .skip(1)
                .all(|list| list.binary_search_by_key(&doc, |pair| pair.0).is_ok());
            if !in_all {
                continue;
            }
            let mut score = 0.0;
            for list in &lists {
                let tf = list
                    .binary_search_by_key(&doc, |pair| pair.0)
                    .ok()
                    .and_then(|i| list.get(i))
                    .map_or(1, |pair| pair.1);
                score += idf(n, list.len() as u64) * norm(tf, *dl, avgdl);
            }
            hits.push(Hit { doc, score });
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.doc.cmp(&b.doc)));
        hits.truncate(limit);
        hits
    }

    /// Nearest-neighbor query over the index's quantized vectors
    /// (`vectors` feature): approximate cosine similarity, best first, at
    /// most `limit` hits. Documents without vectors never match; a query
    /// dimension different from the index's yields no hits (read paths
    /// report emptiness, not errors). Scores are approximate: i8
    /// quantization trades a bounded, fixed error for a format that is a
    /// fraction of the f32 size.
    #[cfg(feature = "vectors")]
    pub fn query_vector(&self, vector: &[f32], limit: usize) -> Vec<Hit> {
        if vector.is_empty() || self.vectors.is_empty() {
            return Vec::new();
        }
        let dim_matches = self
            .vectors
            .first()
            .is_none_or(|(_, q)| q.dims.len() == vector.len());
        if !dim_matches {
            return Vec::new();
        }
        let query = quantize(vector);
        let mut hits: Vec<Hit> = Vec::new();
        for (doc, d) in &self.vectors {
            if d.norm == 0.0 || query.norm == 0.0 {
                continue; // zero vectors carry no direction
            }
            let dot: i64 = d
                .dims
                .iter()
                .zip(&query.dims)
                .map(|(a, b)| i64::from(*a) * i64::from(*b))
                .sum();
            let score = dot as f64 / (f64::from(d.norm) * f64::from(query.norm));
            hits.push(Hit { doc: *doc, score });
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.doc.cmp(&b.doc)));
        hits.truncate(limit);
        hits
    }

    fn postings_for(&self, term: &str) -> Result<Option<PairList>, IndexError> {
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
        read_pairs_at(&self.postings, offset as usize).map(Some)
    }
}

/// Bounds-checked postings-list read at `start`: `u32` byte-length prefix
/// plus (doc u32, tf u16) pairs. Shared by decode-time validation and
/// query-time lookups. Pairs are returned in stored order; the length
/// prefix is exact (`4 + 6 * count` bytes are consumed), which the caller
/// can use to advance a walk.
fn read_pairs_at(postings: &[u8], start: usize) -> Result<PairList, IndexError> {
    let len_pos = start.checked_add(4).ok_or(IndexError::Corrupt {
        reason: "postings offset overflows",
    })?;
    let len_bytes = postings.get(start..len_pos).ok_or(IndexError::Corrupt {
        reason: "postings offset out of range",
    })?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(len_bytes);
    let len = usize::try_from(u32::from_le_bytes(raw)).map_err(|_| IndexError::Corrupt {
        reason: "postings length exceeds address space",
    })?;
    let end = len_pos.checked_add(len).ok_or(IndexError::Corrupt {
        reason: "postings length overflows",
    })?;
    let bytes = postings.get(len_pos..end).ok_or(IndexError::Corrupt {
        reason: "postings bytes out of range",
    })?;
    if len % 6 != 0 {
        return Err(IndexError::Corrupt {
            reason: "postings length is not a multiple of the 6-byte pair",
        });
    }
    let mut out = Vec::with_capacity(len / 6);
    for chunk in bytes.chunks_exact(6) {
        let doc_bytes = chunk.get(0..4).ok_or(IndexError::Corrupt {
            reason: "truncated posting pair",
        })?;
        let tf_bytes = chunk.get(4..6).ok_or(IndexError::Corrupt {
            reason: "truncated posting pair",
        })?;
        let mut doc_raw = [0u8; 4];
        doc_raw.copy_from_slice(doc_bytes);
        let mut tf_raw = [0u8; 2];
        tf_raw.copy_from_slice(tf_bytes);
        out.push((u32::from_le_bytes(doc_raw), u16::from_le_bytes(tf_raw)));
    }
    Ok(out)
}

/// Walk the entire postings area: every entry's length must fit, every
/// pair list must parse, docs must be strictly increasing with tf >= 1
/// and in range, and the entries must exactly cover the area (no trailing
/// garbage, no silently-empty truncation). Decode-time validation means a
/// decoded index can always answer queries; corruption is never deferred
/// to a silent empty result later.
fn validate_postings(postings: &[u8], doc_count: u64) -> Result<(), IndexError> {
    let mut pos = 0usize;
    while pos < postings.len() {
        let list = read_pairs_at(postings, pos)?;
        let mut prev: Option<u32> = None;
        for (doc, tf) in &list {
            if *tf == 0 {
                return Err(IndexError::Corrupt {
                    reason: "posting with zero term frequency",
                });
            }
            if u64::from(*doc) >= doc_count {
                return Err(IndexError::Corrupt {
                    reason: "posting doc id out of range",
                });
            }
            if prev.is_some_and(|p| p >= *doc) {
                return Err(IndexError::Corrupt {
                    reason: "postings not strictly increasing by doc id",
                });
            }
            prev = Some(*doc);
        }
        pos += 4 + list.len() * 6;
    }
    if pos != postings.len() {
        return Err(IndexError::Corrupt {
            reason: "postings not fully consumed",
        });
    }
    Ok(())
}

/// Parse and validate the quantized-vector section (`vectors` feature):
/// exact stride coverage, doc ids strictly increasing and in range,
/// finite scale and norm metadata.
#[cfg(feature = "vectors")]
fn parse_vectors(
    bytes: &[u8],
    dim: u32,
    doc_count: u64,
) -> Result<Vec<(u32, QuantVec)>, IndexError> {
    let dim = usize::try_from(dim).map_err(|_| IndexError::Corrupt {
        reason: "vector dimension exceeds address space",
    })?;
    if dim == 0 {
        return Err(IndexError::Corrupt {
            reason: "vectors section with zero dimension",
        });
    }
    let stride = dim + 12; // doc u32 + scale f32 + norm f32 + dim i8s
    let mut r = Reader::new(bytes);
    let count = r.u32()?;
    let count = usize::try_from(count).map_err(|_| IndexError::Corrupt {
        reason: "vector count exceeds address space",
    })?;
    let expected_len = count
        .checked_mul(stride)
        .and_then(|n| n.checked_add(4))
        .ok_or(IndexError::Corrupt {
            reason: "vectors section length overflows",
        })?;
    if expected_len != bytes.len() {
        return Err(IndexError::Corrupt {
            reason: "vectors section length does not match its count",
        });
    }
    let mut out = Vec::with_capacity(count);
    let mut prev: Option<u32> = None;
    for _ in 0..count {
        let doc = r.u32()?;
        if u64::from(doc) >= doc_count {
            return Err(IndexError::Corrupt {
                reason: "vector doc id out of range",
            });
        }
        if prev.is_some_and(|p| p >= doc) {
            return Err(IndexError::Corrupt {
                reason: "vectors not strictly increasing by doc id",
            });
        }
        prev = Some(doc);
        let scale = r.f32()?;
        let norm = r.f32()?;
        if !scale.is_finite() || !norm.is_finite() || norm < 0.0 {
            return Err(IndexError::Corrupt {
                reason: "non-finite or negative vector scale or norm",
            });
        }
        let mut dims = Vec::with_capacity(dim);
        for _ in 0..dim {
            dims.push(r.i8()?);
        }
        out.push((doc, QuantVec { scale, norm, dims }));
    }
    Ok(out)
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

/// BM25 term-frequency component with length normalization:
/// `tf * (k1 + 1) / (tf + k1 * (1 - b + b * dl / avgdl))`.
fn norm(tf: u16, dl: u64, avgdl: f64) -> f64 {
    let tf = f64::from(tf);
    let dl = dl as f64;
    tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * dl / avgdl))
}
