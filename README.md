# docs-search

Client-side search index for documentation sites: an FST term dictionary
with tf postings and full BM25 ranking, plus an optional quantized-vector
section (the `vectors` feature) for cosine nearest-neighbor queries, all
serialized as a single deterministic binary blob. Bytes-in/bytes-out: a
native docs compiler writes it, a wasm32 browser runtime queries it.
Part of the docs-core extension program (`docs-core/SPEC.md` REQ-Q).

## Module map

| Module | Contents |
|---|---|
| `IndexBuilder` | document ingestion (`add_doc`), deterministic serialization (`build`) |
| `Index` | total decoding (`from_bytes`), AND queries with BM25 (`query`), cosine vectors (`query_vector`, `vectors` feature) |
| `Hit` | one scored result: `doc` id + `score` |
| `IndexError` | typed, exhaustive build/decode errors; decoding never panics |

## Format (v2)

```text
magic   "DSIDX2"
u32     format version (2)
u64     document count
u64     FST byte length
u64     vectors-section byte length (0 when absent)
u32     vector dimension (0 when absent)
u64 * n document byte-lengths
bytes   FST dictionary: term -> postings offset
bytes   postings: concatenated [u32 byte_len][ (doc u32, tf u16) * ]
bytes   vectors (vectors feature): [u32 count][ (doc u32, scale f32, norm f32, dim * i8) * ]
```

Format 1 (DSIDX1, presence bitmaps, unit tf) is retired; a v1 blob fails
decoding with `InvalidMagic`. Quantization is fixed max-abs scaling into
i8 with per-vector scale + norm - deterministic arithmetic, no model.

Serialization is byte-for-byte reproducible for the same input; the query
half treats the whole blob as untrusted input (bounds-checked, typed
errors, total: see the corruption-sweep and property tests).

## Scope (0.2, honest)

- Ranking is **full BM25** over true term-frequency postings: the 0.1
  unit-tf placeholder is retired, and repetition now matters (saturated
  by `k1 = 1.2`) alongside the length norm.
- Queries are boolean AND; the `vectors` feature adds approximate-cosine
  nearest-neighbor over the quantized section (documents without vectors
  never match; dimension mismatches report emptiness, not errors).
- Phrase queries, field weights, positional postings, and learned-model
  codecs do not exist. Text extraction, snippets, and highlight spans are
  the compiler's job; this crate stores terms, frequencies, lengths, and
  (optionally) quantized vectors.

## Layer

L0 - leaf. Zero estate-internal dependencies. External floor: `fst`,
`thiserror` (`roaring` left with the bitmap postings in 0.2). Builds
unmodified for `wasm32-unknown-unknown`, default features and `vectors`
both (CI enforces the default build; the feature build is checked
locally per release).

## Tests

`cargo test` (golden-path, corruption sweep, truncation/flip property
tests) under clippy `-D warnings` with the estate decoder-pattern denies
(`unwrap`/`expect`/`panic`/`indexing_slicing` lifted in test targets only).

Licensed under MIT OR Apache-2.0.
