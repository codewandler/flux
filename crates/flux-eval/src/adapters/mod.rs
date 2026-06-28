//! Concrete benchmark adapters.
//!
//! [`local::LocalAdapter`] provides the offline `mock` suite (a CI fixture). Real benchmarks run
//! through [`terminal_bench::TerminalBenchAdapter`]; a SWE-bench Lite adapter will arrive behind the
//! same [`BenchmarkAdapter`](crate::adapter::BenchmarkAdapter) trait.

pub mod local;
pub mod multi;
pub mod terminal_bench;

pub use local::LocalAdapter;
pub use multi::MultiAdapter;
pub use terminal_bench::TerminalBenchAdapter;
