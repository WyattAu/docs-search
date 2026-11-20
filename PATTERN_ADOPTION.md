# Pattern adoption

docs-search is the L0 client-side search index crate: FST term
dictionary, BM25 postings, optional quantized vectors, bytes-in and
bytes-out wasm32-clean format. Declared per references/adoption-format.md
in pattern-standards.

| Pattern | Status | Evidence |
|---|---|---|
| P64 | adopted | tests/property.rs:17 (proptest suite over arbitrary corruption: decode totality, 256 cases per run) |
| T40 | adopted | src/format.rs:88 (the reader parses the byte buffer in place and returns subslices; fixed little-endian framing, no serde layer) |
