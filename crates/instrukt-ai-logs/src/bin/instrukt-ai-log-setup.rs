//! `instrukt-ai-log-setup`: wire per-user log rotation.

fn main() {
    std::process::exit(instrukt_ai_logs::setup::run(std::env::args_os()).report());
}
