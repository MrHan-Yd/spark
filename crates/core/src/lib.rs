//! Spark core: domain types and pure logic (testable without Win32).

mod candidate;
mod error;
mod paths;
mod query;
mod rank;

pub use candidate::{Action, Candidate, IconRef, Source};
pub use error::CoreError;
pub use paths::{config_path, data_dir, ensure_data_dir, history_path};
pub use query::Query;
pub use rank::rank_candidates;

pub const APP_NAME: &str = "Spark";
pub const APP_ID: &str = "app.spark.launcher";
pub const SINGLE_INSTANCE_MUTEX: &str = "Local\\SparkLauncherHost_v1";
