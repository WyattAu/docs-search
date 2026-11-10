//! Golden-path tests: build, decode, query, determinism.

// Test-only lifts (docs-core decoder-pattern precedent): corruption tests
// slice and panic on purpose; expectations carry context on the
// build/decode halves of each test. Library paths keep the denies.
#![allow(clippy::indexing_slicing, clippy::panic, clippy::expect_used)]

use docs_search::{Index, IndexBuilder, IndexError};

fn sample_index() -> Vec<u8> {
    let mut b = IndexBuilder::new();
    b.add_doc("the quick brown fox jumps over the lazy dog");
    b.add_doc("quick brown designs");
    b.add_doc("a slow yellow fox naps");
    b.add_doc("unrelated text about tulips");
    b.build().expect("sample index builds")
}

#[test]
fn round_trip_and_query() {
    let bytes = sample_index();
    let idx = Index::from_bytes(&bytes).expect("decodes");
    assert_eq!(idx.len(), 4);

    let hits = idx.query("fox", 10);
    assert_eq!(hits.len(), 2, "fox matches docs 0 and 2");
    let docs: Vec<u32> = hits.iter().map(|h| h.doc).collect();
    assert!(docs.contains(&0) && docs.contains(&2));

    assert_eq!(idx.query("quick brown", 10).len(), 2, "AND over docs 0,1");
    assert_eq!(idx.query("tulips", 10).len(), 1);
}

#[test]
fn and_semantics_drops_partial_matches() {
    let bytes = sample_index();
    let idx = Index::from_bytes(&bytes).expect("decodes");
    // "lazy" only in doc 0; "tulips" only in doc 3: AND is empty.
    assert!(idx.query("lazy tulips", 10).is_empty());
}

#[test]
fn unknown_terms_and_empty_query_yield_no_hits() {
    let bytes = sample_index();
    let idx = Index::from_bytes(&bytes).expect("decodes");
    assert!(idx.query("zebra", 10).is_empty());
    assert!(idx.query("", 10).is_empty());
    assert!(idx.query("!!! ...", 10).is_empty(), "no alphanumeric terms");
}

#[test]
fn serialization_is_deterministic() {
    let one = sample_index();
    let two = sample_index();
    assert_eq!(one, two, "same input, identical bytes");
}

#[test]
fn ranking_prefers_shorter_docs_on_equal_terms() {
    let mut b = IndexBuilder::new();
    b.add_doc("quick quick quick quick quick quick quick quick");
    b.add_doc("quick");
    let bytes = b.build().expect("builds");
    let idx = Index::from_bytes(&bytes).expect("decodes");
    let hits = idx.query("quick", 10);
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0].doc, 1,
        "unit-tf BM25 still ranks the shorter doc first"
    );
}

#[test]
fn every_truncation_is_a_typed_error_not_a_panic() {
    let bytes = sample_index();
    // Every proper prefix must fail: `..len` excludes the complete blob,
    // which is the one cut that legitimately decodes.
    for cut in 0..bytes.len() {
        let partial = &bytes[..cut];
        match Index::from_bytes(partial) {
            Ok(_) => panic!("truncated at {cut} must not decode"),
            Err(IndexError::Fst(_)) => {
                panic!("fst error at {cut} must surface as a docs_search error")
            }
            Err(_) => {}
        }
    }
}

#[test]
fn bad_magic_is_invalid_magic() {
    let mut bytes = sample_index();
    bytes[0] = b'X';
    assert!(matches!(
        Index::from_bytes(&bytes),
        Err(IndexError::InvalidMagic { .. })
    ));
}

#[test]
fn newer_version_is_rejected() {
    let mut bytes = sample_index();
    // version field sits at bytes 6..10, little-endian.
    bytes[6] = 99;
    assert!(matches!(
        Index::from_bytes(&bytes),
        Err(IndexError::UnsupportedVersion { found: 99, .. })
    ));
}
