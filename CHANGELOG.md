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
