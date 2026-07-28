//! Checkout-owned execution provider adapter.

mod runner;

pub(crate) use runner::{
    compile_automatic_package, compile_local_command_package, compile_package,
    compile_package_automatically, execute, locate_package, AutomaticCompilation,
    LocalCommandObservation, LocalCommandPlan, PlanRun,
};
