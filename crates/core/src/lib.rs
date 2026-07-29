//! Spark core: domain types and pure logic (testable without Win32).

mod candidate;
mod error;
mod query;
mod rank;

pub use candidate::{Action, Candidate, IconRef, Source};
pub use error::CoreError;
pub use query::Query;
pub use rank::rank_candidates;

pub const APP_NAME: &str = "Spark";
pub const APP_ID: &str = "app.spark.launcher";
