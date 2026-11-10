//! docs-search: the client-side search index for docs sites
//! (docs-core extension program, `SPEC.md` REQ-Q).
//!
//! A serialized index is a single binary blob: an FST term dictionary whose
//! values are offsets into a postings area of roaring bitmaps, plus a
//! document-length table for BM25-shaped scoring. The whole API is
//! bytes-in/bytes-out, so the same artifact serves a native compiler
//! writing it and a browser runtime (wasm32) querying it.
//!
//! ```
//! use docs_search::{IndexBuilder, Index};
//!
//! let mut b = IndexBuilder::new();
//! b.add_doc("the quick brown fox");
//! b.add_doc("the lazy dog");
//! let bytes = b.build().unwrap();
//!
//! let idx = Index::from_bytes(&bytes).unwrap();
//! let hits = idx.query("fox", 10);
//! assert_eq!(hits.len(), 1);
//! assert_eq!(hits[0].doc, 0);
//! ```
//!
//! # Scope (0.1, honest)
//!
//! In: boolean AND queries, BM25-shaped ranking with unit term frequency
//! (postings are presence bitmaps; frequency counts are deferred, and the
//! ranking documents that), deterministic serialization, total decoding.
//! Out: phrase queries, field weights, positional postings, and the
//! quantized-vector section (REQ-Q1 keeps it behind a feature flag; it
//! lands in a later 0.x, not silently: it is absent from the format's
//! flag word until then).
//!
//! # Layer
//!
//! L0 - leaf. Zero estate-internal dependencies; external floor is
//! `fst`, `roaring`, `thiserror`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod error;
mod format;
mod index;

pub use error::IndexError;
pub use index::{Hit, Index, IndexBuilder};

/// The crate's version, for callers that report it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
