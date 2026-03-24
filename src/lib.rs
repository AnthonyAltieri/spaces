pub mod app;
pub mod cli;
pub mod git;
pub mod names;
pub mod registry;

use std::io::{self, BufReader};

pub fn run() -> anyhow::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let mut output = stdout.lock();

    cli::run_from(std::env::args_os(), &mut input, &mut output)
}
