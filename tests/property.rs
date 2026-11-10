//! Property tests: decode totality under arbitrary corruption.

// Test-only lifts (docs-core decoder-pattern precedent): slicing is the
// point of the corruption properties; expectations carry context in the
// setup half of each property. Library paths keep the denies.
#![allow(clippy::indexing_slicing, clippy::expect_used)]

use docs_search::{Index, IndexBuilder, IndexError};
use proptest::prelude::*;

/// Lift a library Result into a proptest outcome; build/decode failures in
/// these scenarios are bugs, not test failures of the property.
fn lib_ok<T>(r: Result<T, IndexError>) -> Result<T, TestCaseError> {
    r.map_err(|e| TestCaseError::fail(e.to_string()))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn corrupted_input_never_panics(
        cut in 0usize..400,
        bitflip in 0usize..400,
    ) {
        let mut b = IndexBuilder::new();
        b.add_doc("alpha beta gamma");
        b.add_doc("beta delta epsilon zeta eta theta");
        let mut bytes = lib_ok(b.build())?;

        let cut = cut.min(bytes.len());
        bytes.truncate(cut);
        if let Some(slot) = bytes.get_mut(bitflip) {
            *slot ^= 0xff;
        }
        // The only contract: decoding is total. Ok or Err, never panic.
        let _ = Index::from_bytes(&bytes);
    }

    #[test]
    fn round_trip_preserves_membership(
        docs in proptest::collection::vec("[a-zA-Z0-9 ]{1,64}", 1..12),
    ) {
        let mut b = IndexBuilder::new();
        for d in &docs {
            b.add_doc(d);
        }
        let bytes = lib_ok(b.build())?;
        let idx = lib_ok(Index::from_bytes(&bytes))?;
        prop_assert_eq!(idx.len() as usize, docs.len());
        // First word of doc 0 must surface doc 0 in an AND query.
        let first: String = docs[0]
            .split(|c: char| !c.is_alphanumeric())
            .find(|t| !t.is_empty())
            .unwrap_or_default()
            .to_lowercase();
        if !first.is_empty() {
            prop_assert!(idx.query(&first, docs.len()).iter().any(|h| h.doc == 0));
        }
    }
}
