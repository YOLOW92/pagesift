//! Adaptive hardware sensing, dynamic AIMD concurrency control, lock-free bitsets, and memory budgeting.

pub mod bitset;
pub mod budget;
pub mod controller;
pub mod telemetry;

pub use bitset::AtomicBitSet;
pub use budget::{BudgetPermit, InflightBudget};
pub use controller::{AimdController, ControllerConfig, ControllerState};
pub use telemetry::{AdaptiveSchedule, HardwareTopology, MemorySnapshot, TopologyCategory};
