use std::sync::{Arc, atomic::AtomicBool};

use clap::Parser;
use luchs::{Result, cli::Cli};

fn run() -> Result<()> {
    let cli = Cli::parse();
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, stop.clone())?;
    }
    luchs::source::run(cli, stop)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("luchs: {error}");
        std::process::exit(1);
    }
}
