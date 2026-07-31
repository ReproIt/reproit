//! Checkout-owned execution provider adapter.

pub(crate) mod runner;

pub(crate) use runner::{
    compile_automatic_package, compile_local_command_package, compile_package_automatically,
    execute, locate_package, persist_plan_catalog, AutomaticCompilation, CompilationBlocker,
    LocalCommandObservation, LocalCommandPlan, PlanRun,
};
