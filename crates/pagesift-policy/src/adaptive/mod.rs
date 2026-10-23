//! Adaptive hardware sensing, dynamic AIMD concurrency control, lock-free bitsets, and memory budgeting.

pub mod bitset;
pub mod budget;
pub mod controller;
pub mod knobs;
pub mod surface;
pub mod telemetry;

pub use bitset::AtomicBitSet;
pub use budget::{BudgetPermit, InflightBudget};
pub use controller::{
    AimdController, AutonomousTuningEngine, ControllerConfig, ControllerState,
    PerformanceDiagnosis,
};
pub use knobs::{AtomicTuningKnobs, TuningKnobs};
pub use surface::{ResponseSurfaceOptimizer, SafetyStatus, SurfacePoint};
pub use telemetry::{
    AdaptiveSchedule, CpuSaturationTracker, HardwareTopology, MemorySnapshot,
    QuantileLatencyTracker, RssDerivativeTracker, TopologyCategory,
};
