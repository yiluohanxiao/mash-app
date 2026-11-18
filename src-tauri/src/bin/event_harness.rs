//! Thin entry for the headless event-farm device harness; all logic lives
//! in `mash_lib::event_harness` so it can use the crate-internal scanner
//! and farm primitives.

fn main() {
    std::process::exit(mash_lib::event_harness::run_from_env());
}
