//! Typed, exhaustive errors for index build and decode.

use std::fmt;

/// Everything that can go wrong building or reading an index.
#[derive(Debug)]
#[non_exhaustive]
pub enum IndexError {
    /// The blob does not start with the format magic.
    InvalidMagic {
        /// The first bytes found, for diagnosis; never assumed valid UTF-8.
        found: [u8; 6],
    },
    /// The format version is newer than this crate understands.
    UnsupportedVersion {
        /// The version read from the header.
        found: u32,
        /// The highest version this crate decodes.
        supported: u32,
    },
    /// The blob ends before a section the header promises.
    Truncated {
        /// What was being read when the blob ran out.
        section: &'static str,
    },
    /// A section's internal structure is inconsistent (bad offset, bad
    /// bitmap bytes, overflowing count).
    Corrupt {
        /// What was inconsistent, for diagnosis.
        reason: &'static str,
    },
    /// The FST dictionary itself failed to load.
    Fst(fst::Error),
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic { found } => {
                write!(f, "invalid magic: expected \"DSIDX1\", found {found:02x?}")
            }
            Self::UnsupportedVersion { found, supported } => {
                write!(
                    f,
                    "index version {found} unsupported; this crate decodes up to {supported}"
                )
            }
            Self::Truncated { section } => {
                write!(f, "truncated index: ran out of bytes reading {section}")
            }
            Self::Corrupt { reason } => write!(f, "corrupt index: {reason}"),
            Self::Fst(e) => write!(f, "term dictionary error: {e}"),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fst(e) => Some(e),
            _ => None,
        }
    }
}

impl From<fst::Error> for IndexError {
    fn from(e: fst::Error) -> Self {
        Self::Fst(e)
    }
}
