//! Core logic of the jsc test262 conformance runner: frontmatter parsing,
//! eligibility filtering, program assembly, outcome classification, baseline
//! diffing, test discovery and report aggregation. Process execution lives in
//! the binary.

pub mod assemble;
pub mod baseline;
pub mod filter;
pub mod frontmatter;
pub mod outcome;
pub mod report;
pub mod walk;
