//! Concrete benchmark adapters.
//!
//! M1 ships [`local::LocalAdapter`] (which also provides the offline `mock` suite). The Docker-based
//! terminal-bench and SWE-bench Lite adapters arrive at M5 behind the same
//! [`BenchmarkAdapter`](crate::adapter::BenchmarkAdapter) trait.

pub mod local;
pub mod terminal_bench;

pub use local::LocalAdapter;
pub use terminal_bench::TerminalBenchAdapter;
