//! 1.5 Loop 持续数据同步引擎：资源清单生命周期、价值加权新鲜度调度与 5 步闭环执行。

mod budget;
mod capture;
pub mod inventory;
mod reconcile;
pub mod runner;
pub mod scheduler;

pub use inventory::{
    InventoryObservation, apply_observation_to_record, classify_resource_role,
    ensure_seeds_in_inventory, normalize_in_scope_url, prune_out_of_scope_inventory,
};
pub use runner::SyncRunOptions;
pub use scheduler::{ScheduledCandidate, compute_next_due_at_ms, select_tick_candidates};
