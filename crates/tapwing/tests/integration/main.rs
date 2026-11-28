//! Single integration test binary housing Tapwing's suite modules.

mod cli_e2e;
mod common;
mod compat;
mod f6_sharding_and_reports;
mod f7_app_health;
mod f8_failure_reports;
mod f8_trace;
mod fast_inner_loop;
mod init;
mod integration_core;
mod integration_orchestrator;
mod report_edge_cases;
mod workspace;
