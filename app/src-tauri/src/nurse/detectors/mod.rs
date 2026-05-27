//! Concrete `Detector` implementations.
//!
//! See `nurse/detector.rs` for the trait contract.

pub mod context_saturation;
pub mod process_health;
pub mod provider_health;
pub mod reasoning_loop;
pub mod retry_exhaustion;
pub mod stall;
pub mod tool_failure;

pub use context_saturation::ContextSaturationDetector;
pub use process_health::ProcessHealthDetector;
pub use provider_health::ProviderHealthDetector;
pub use reasoning_loop::ReasoningLoopDetector;
pub use retry_exhaustion::RetryExhaustionDetector;
pub use stall::StallDetector;
pub use tool_failure::ToolFailureDetector;
