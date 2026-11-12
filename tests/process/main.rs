#[path = "../common/mod.rs"]
mod common;

mod atomic_lease_acquisition_test;
mod bundled_binaries_test;
mod claude_hook_events_test;
mod closed_stdout_test;
mod concurrency_boundaries_test;
mod ignore_surface_concurrency_test;
mod landing_claim_contention_test;
mod main_thread_stack_test;
mod owned_ledger_concurrency_test;
mod parallel_test;
mod plugins_dispatch_test;
mod plugins_help_doctor_test;
mod push_parallel_test;
mod rwv_each_test;
mod rwv_each_vocabulary_test;
mod tool_only_bin_race_test;
mod version_flag_test;
mod workweave_index_concurrency_test;
