# docs-search

Client-side search index for documentation sites: an FST term dictionary
with roaring-bitmap postings and BM25-shaped ranking, serialized as a
single deterministic binary blob. Bytes-in/bytes-out: a native docs
compiler writes it, a wasm32 browser runtime queries it. Part of the
docs-core extension program (`docs-core/SPEC.md` REQ-Q).

## Module map

| Module | Contents |
|---|---|
| `IndexBuilder` | document ingestion (`add_doc`), deterministic serialization (`build`) |
| `Index` | total decoding (`from_bytes`), AND queries with BM25-shaped ranking (`query`) |
| `Hit` | one scored result: `doc` id + `score` |
| `IndexError` | typed, exhaustive build/decode errors; decoding never panics |

## Format (v1)

```text
magic   "DSIDX1"
u32     format version
u64     document count
u64     FST byte length
u64 * n document byte-lengths
bytes   FST dictionary: term -> postings offset
bytes   postings: concatenated [u32 len][roaring bitmap]
```

Serialization is byte-for-byte reproducible for the same input; the query
half treats the whole blob as untrusted input (bounds-checked, typed
errors, total: see the corruption-sweep and property tests).

## Scope (0.1, honest)

- Ranking is BM25-shaped with **unit term frequency**: postings are
  presence bitmaps, so `tf = 1`. Frequency counts (positional or delta
  postings) are the next 0.x increment, not silently folded in.
- Queries are boolean AND. Phrase queries, field weights, and the
  quantized-vector section (REQ-Q1's feature-flagged part) do not exist
  yet; the format's flag word is reserved for them.
- Text extraction, snippets, and highlight spans are the compiler's job;
  this crate stores term presence and document lengths only.

## Layer

L0 - leaf. Zero estate-internal dependencies. External floor: `fst`,
`roaring`, `thiserror`. Builds unmodified for `wasm32-unknown-unknown`
(CI enforces it).

## Tests

`cargo test` (golden-path, corruption sweep, truncation/flip property
tests) under clippy `-D warnings` with the estate decoder-pattern denies
(`unwrap`/`expect`/`panic`/`indexing_slicing` lifted in test targets only).

Licensed under MIT OR Apache-2.0.
