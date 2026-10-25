//! Adaptive hardware sensing, dynamic AIMD concurrency control, lock-free bitsets, and memory budgeting.

pub mod bitset;
pub mod budget;
pub mod capacity;
pub mod controller;
pub mod distribution;
pub mod evidence;
pub mod gate;
pub mod knobs;
pub mod pipeline;
pub mod surface;
pub mod telemetry;
pub mod topology;

pub use bitset::AtomicBitSet;
pub use budget::{BudgetPermit, InflightBudget};
pub use capacity::{
    AdaptiveCapacityController, CapacityControllerConfig, CapacityDecision, CapacityPhase,
    CapacitySample, ConcurrencyMeasurement, WorkerTierMeasurement,
};
pub use controller::{
    AimdController, AutonomousTuningEngine, ControllerConfig, ControllerState, PerformanceDiagnosis,
};
pub use distribution::{
    DistributionAllocation, DistributionControlState, DistributionController,
    DistributionControllerConfig, DistributionSample,
};
pub use evidence::{SampleWindow, WindowEvidence};
pub use gate::{AdaptiveConcurrencyGate, AdaptiveConcurrencyPermit};
pub use knobs::{AtomicTuningKnobs, TuningKnobs};
pub use pipeline::{
    PipelineAllocation, PipelineControlState, PipelineController, PipelineControllerConfig,
    PipelineSample,
};
pub use surface::{ResponseSurfaceOptimizer, SafetyStatus, SurfacePoint};
pub use telemetry::{
    AdaptiveSchedule, CpuSaturationTracker, HardwareTopology, MemorySnapshot, ProcessCpuSample,
    QuantileLatencyTracker, RssDerivativeTracker, TopologyCategory,
};
pub use topology::{
    CpuThread, CpuTopology, LocalityCluster, pin_current_thread, pin_current_thread_to_set,
    set_process_affinity,
};
