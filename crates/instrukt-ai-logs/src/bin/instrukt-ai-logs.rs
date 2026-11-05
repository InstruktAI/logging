//! `instrukt-ai-logs`: read and merge an app's logs.

fn main() {
    std::process::exit(instrukt_ai_logs::reader::run(std::env::args_os()).report());
}
