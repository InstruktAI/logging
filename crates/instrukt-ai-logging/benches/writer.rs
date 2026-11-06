//! Wall-clock / CodSpeed benchmarks of the writer: one log call per iteration.
//!
//! Run locally with `cargo bench -p instrukt-ai-logging --bench writer`; under
//! CodSpeed with `cargo codspeed build -p instrukt-ai-logging --bench writer`
//! and `cargo codspeed run`. The cases live in `cases/mod.rs`.

mod cases;

fn main() {
    divan::main();
}
