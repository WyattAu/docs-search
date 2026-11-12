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
fn ranking_rewards_repetition_up_to_saturation() {
    let mut b = IndexBuilder::new();
    b.add_doc("quick quick quick quick quick quick quick quick");
    b.add_doc("quick");
    let bytes = b.build().expect("builds");
    let idx = Index::from_bytes(&bytes).expect("decodes");
    let hits = idx.query("quick", 10);
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0].doc, 0,
        "tf 8 outranks tf 1 even with the shorter doc's length bonus: \
         real frequency signal (0.2) beats the unit-tf placeholder"
    );
    // Saturation: k1 = 1.2 means tf 8 is worth less than 8x tf 1.
    assert!(
        hits[0].score < 8.0 * hits[1].score,
        "repetition saturates: score(tf 8) < 8 * score(tf 1)"
    );
}

#[test]
fn ranking_prefers_shorter_docs_at_equal_tf() {
    let mut b = IndexBuilder::new();
    b.add_doc("quick");
    b.add_doc("quick text lengthener word padding");
    let bytes = b.build().expect("builds");
    let idx = Index::from_bytes(&bytes).expect("decodes");
    let hits = idx.query("quick", 10);
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0].doc, 0,
        "equal tf: the BM25 length norm ranks the shorter doc first"
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

/// A vectors-carrying index, for the vectors-feature halves below.
#[cfg(feature = "vectors")]
fn vector_index() -> Vec<u8> {
    let mut b = IndexBuilder::new();
    let d0 = b.add_doc("alpha search engine");
    let d1 = b.add_doc("beta storage engine");
    let d2 = b.add_doc("gamma unrelated prose");
    b.set_vector(d0 as u32, &[1.0, 0.0, 0.0]).expect("v0");
    b.set_vector(d1 as u32, &[0.9, 0.1, 0.0]).expect("v1");
    b.set_vector(d2 as u32, &[0.0, 0.0, 1.0]).expect("v2");
    b.build().expect("builds")
}

#[cfg(feature = "vectors")]
#[test]
fn vector_query_ranks_by_approximate_cosine() {
    let idx = Index::from_bytes(&vector_index()).expect("decodes");
    let hits = idx.query_vector(&[1.0, 0.0, 0.0], 10);
    assert_eq!(hits.len(), 3, "every vectored doc answers");
    assert_eq!(hits[0].doc, 0, "exact direction ranks first");
    assert_eq!(hits[1].doc, 1, "nearby direction ranks second");
    assert!(hits[0].score > 0.99, "exact direction: cosine ~1");
    assert!(
        hits[1].score > 0.5 && hits[1].score < 1.0,
        "nearby direction: cosine between"
    );
    assert_eq!(hits[2].doc, 2, "orthogonal direction ranks last");
}

#[cfg(feature = "vectors")]
#[test]
fn vector_query_reports_emptiness_on_dim_mismatch() {
    let idx = Index::from_bytes(&vector_index()).expect("decodes");
    assert!(
        idx.query_vector(&[1.0, 0.0], 10).is_empty(),
        "wrong dimension yields no hits, not an error"
    );
    assert!(idx.query_vector(&[], 10).is_empty(), "empty query vector");
}

#[cfg(feature = "vectors")]
#[test]
fn set_vector_rejects_mismatched_dims_and_unknown_docs() {
    let mut b = IndexBuilder::new();
    let d0 = b.add_doc("alpha");
    b.set_vector(d0 as u32, &[1.0, 0.0])
        .expect("first fixes dim");
    assert!(matches!(
        b.set_vector(d0 as u32, &[1.0, 0.0, 0.0]),
        Err(IndexError::VectorDim {
            expected: 2,
            found: 3,
        })
    ));
    assert!(
        matches!(
            b.set_vector(99, &[1.0, 0.0]),
            Err(IndexError::Corrupt { .. })
        ),
        "no doc 99 exists"
    );
    assert!(
        matches!(
            b.set_vector(d0 as u32, &[]),
            Err(IndexError::Corrupt { .. })
        ),
        "empty vectors carry no direction"
    );
}

#[cfg(feature = "vectors")]
#[test]
fn zero_vectors_and_vectorless_docs_never_match() {
    let mut b = IndexBuilder::new();
    let d0 = b.add_doc("alpha zero vector");
    let _ = b.add_doc("beta no vector");
    b.set_vector(d0 as u32, &[0.0, 0.0])
        .expect("zero vector accepted");
    let bytes = b.build().expect("builds");
    let idx = Index::from_bytes(&bytes).expect("decodes");
    assert!(
        idx.query_vector(&[1.0, 1.0], 10).is_empty(),
        "zero-vector docs carry no direction; vectorless docs are invisible"
    );
}

#[cfg(not(feature = "vectors"))]
#[test]
fn vector_sections_are_rejected_without_the_feature() {
    // Build a vectors-carrying blob by hand in the documented layout so
    // this test compiles without the feature: header (docs=1, fst_len,
    // vec_len, dim=1), doc table, empty FST is not needed - a minimal
    // blob with a vectors section is enough to reach the check.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"DSIDX2");
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&1u64.to_le_bytes()); // docs
    bytes.extend_from_slice(&0u64.to_le_bytes()); // fst_len
    bytes.extend_from_slice(&17u64.to_le_bytes()); // vec_len
    bytes.extend_from_slice(&1u32.to_le_bytes()); // dim
    bytes.extend_from_slice(&1u64.to_le_bytes()); // doc_lens[0]
                                                  // vectors: count=1, doc=0, scale=1.0, norm=1.0, one dim (i8 127)
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&1.0f32.to_le_bytes());
    bytes.extend_from_slice(&1.0f32.to_le_bytes());
    bytes.push(127);
    assert_eq!(bytes.len(), 38 + 8 + 17, "hand-built layout is exact");
    assert!(matches!(
        Index::from_bytes(&bytes),
        Err(IndexError::VectorsUnsupported)
    ));
}
