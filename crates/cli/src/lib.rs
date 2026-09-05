//! Library surface of `acb-cli`. Exposes the operator-side helpers that
//! warrant their own unit tests; the daemon-facing subcommands stay in the
//! binary crate (`main.rs`).

pub mod edit;
pub mod sign;
