//! Checkout-owned execution provider adapter.

pub(crate) mod runner;

pub(crate) use runner::{
    compile_automatic_package, compile_local_command_package, compile_package_automatically,
    execute, locate_package, persist_plan_catalog, pinned_provider_digest, repin_guard_providers,
    repin_package_mechanism, source_digest, AutomaticCompilation, CompilationBlocker,
    LocalCommandObservation, LocalCommandPlan, PlanRun,
};
