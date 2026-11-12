# Changelog

All notable changes to this project are documented in this file.

## [0.1.0] - 2026-11-10

### Added

- `IndexBuilder`: document ingestion and deterministic binary
  serialization (FST dictionary + roaring-bitmap postings + doc-length
  table; format version 1).
- `Index`: total decoding of untrusted blobs (typed errors, no panics)
  and AND queries ranked by BM25-shaped scoring with unit term frequency.
- Property tests: decode totality under truncation/bit-flip corruption.
- wasm32-clean build enforced in CI (`wasm32-unknown-unknown` target).

## [0.2.0] - 2026-11-12

### Changed

- **Breaking (0.x): format v2, magic `DSIDX2`.** Postings are now
  (doc u32, tf u16) pair lists instead of roaring bitmaps, so ranking is
  full BM25 over true term frequencies - the 0.1 unit-tf placeholder is
  retired and repetition affects scores (saturated at `k1 = 1.2`). A v1
  blob fails decoding with `InvalidMagic`. `roaring` leaves the
  dependency floor (now `fst` + `thiserror`).
- `add_doc` returns the document id (the insertion ordinal).

### Added

- `vectors` feature (REQ-Q1's feature-flagged slice): `set_vector`
  quantizes an f32 embedding per-vector (fixed max-abs scaling into i8,
  stored with scale + norm), `build` emits a count-prefixed vectors
  section behind new fixed-header fields (`vec_len`, `dim`), and
  `query_vector` answers with approximate cosine over it. Docs without
  vectors never match; dimension mismatches report emptiness. Decoding a
  vectors-carrying blob without the feature is a typed
  `VectorsUnsupported` error; corrupt sections (stride mismatch,
  non-increasing doc ids, non-finite metadata) are `Corrupt`.
- `IndexError::VectorsUnsupported` and `IndexError::VectorDim`.
- Vector corruption property test (256 cases of truncation + bitflip
  over a vectors-carrying blob: decode stays total).
