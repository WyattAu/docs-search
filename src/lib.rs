//! docs-search: the client-side search index for docs sites
//! (docs-core extension program, `SPEC.md` REQ-Q).
//!
//! A serialized index is a single binary blob: an FST term dictionary
//! whose values are offsets into a postings area of (doc, tf) pair
//! lists, a document-length table for BM25, and - behind the `vectors`
//! feature - a quantized-vector section for cosine nearest-neighbor
//! queries. The whole API is bytes-in/bytes-out, so the same artifact
//! serves a native compiler writing it and a browser runtime (wasm32)
//! querying it.
//!
//! ```
//! use docs_search::{Index, IndexBuilder};
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
//! # Scope (0.2, honest)
//!
//! In: boolean AND queries, full BM25 ranking over true term-frequency
//! postings (the 0.1 unit-tf placeholder retired), deterministic
//! serialization, total decoding, and - behind the `vectors` feature -
//! fixed max-abs i8 quantization with an approximate-cosine query.
//! Out: phrase queries, field weights, positional postings, and any
//! learned-model codec (the quantization scheme is fixed arithmetic, not
//! a model).
//!
//! # Layer
//!
//! L0 - leaf. Zero estate-internal dependencies; external floor is
//! `fst` and `thiserror` (`roaring` left with the bitmap postings in
//! 0.2; pair lists need nothing beyond the core).

#![deny(missing_docs)]
// `unsafe` is denied everywhere except src/wasm.rs — the wasm32-only
// C ABI module (browser reader), where raw pointer crossings are the
// interface itself. That module re-allows locally and documents every use.
#![deny(unsafe_code)]

mod error;
mod format;
mod index;

#[cfg(target_arch = "wasm32")]
mod wasm;

pub use error::IndexError;
pub use index::{Hit, Index, IndexBuilder};

/// The crate's version, for callers that report it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
