//! Core logic of the jsc test262 conformance runner: frontmatter parsing,
//! eligibility filtering, program assembly, outcome classification, baseline
//! diffing, sharding, test discovery, report aggregation and process execution.

pub mod assemble;
pub mod baseline;
pub mod exec;
pub mod filter;
pub mod frontmatter;
pub mod outcome;
pub mod partition;
pub mod report;
pub mod walk;
