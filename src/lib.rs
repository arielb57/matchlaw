//! matchlaw: an executable specification for continuous limit-order matching.
//!
//! - [`fast::FastEngine`]: price-level `BTreeMap`s holding FIFO queues.
//! - [`oracle::Oracle`]: a flat `Vec` re-sorted and scanned on every match.
//! - [`check::Checker`]: replays an engine's recorded output against the oracle and names
//!   the first broken [`semantics::Rule`].
//! - [`gen`]: seeded adversarial stream generator; [`hunt`]: find and shrink failing streams.
//! - [`mutants::Mutation`]: deliberately broken variants of the fast engine.

pub mod check;
pub mod csv;
pub mod fast;
pub mod gen;
pub mod hunt;
pub mod mutants;
pub mod oracle;
pub mod semantics;
pub mod types;

pub use check::{replay, Checker, Divergence};
pub use fast::FastEngine;
pub use oracle::Oracle;
pub use semantics::Rule;
pub use types::*;
