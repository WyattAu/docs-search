# Security Policy

## Reporting a vulnerability

Email wyatt_au@protonmail.com with the repo name in the subject. Please
include reproduction steps or a failing input; do not open a public issue
for an unfixed vulnerability.

## Content discipline

A serialized index is treated as untrusted input (the estate decoder
pattern): decoding is total, every byte access is bounds-checked, and
malformed blobs produce typed errors (`IndexError`), never panics. The
doc-length table is size-bounded by the remaining blob length, so a
hostile header cannot trigger an allocation bomb. If you find an input
that panics, hangs, or allocates unboundedly, that is a security bug.

Known upstream limitation: `fst` 0.4.7 panics internally on hostile
dictionaries (out-of-bounds / subtract-overflow in its node walker; found
by this crate's corruption property test). This crate wraps every fst
entry point in an unwind boundary and converts panics to typed errors;
do not build this crate with `panic = "abort"`. Upstreaming a fix to fst
is noted as future work.

## Supported versions

Only the latest released `0.1.x` is supported with fixes.
