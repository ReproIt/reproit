//! Checkout-owned execution provider adapter.

mod runner;

pub(crate) use runner::{
    compile_automatic_package, compile_local_command_package, compile_package, execute,
    locate_package, LocalCommandObservation, LocalCommandPlan, PlanRun,
};
