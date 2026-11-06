//! The writer cases again, with divan's counting allocator installed, to report
//! heap allocations per log call (`cargo bench -p instrukt-ai-logging --bench
//! writer_allocs`). Kept apart from `writer` so the counting allocator never
//! skews the timings there. CodSpeed's `memory` instrument covers this in CI.

#[path = "cases/mod.rs"]
mod cases;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}
