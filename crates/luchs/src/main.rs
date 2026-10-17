use std::sync::{Arc, atomic::AtomicBool};

use clap::Parser;
use luchs::{Result, cli::Cli};

fn run() -> Result<()> {
    let cli = Cli::parse();
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, stop.clone())?;
    }
    if cli.renderer == "cef" {
        #[cfg(all(feature = "cef", target_os = "macos"))]
        return luchs::cef::run(cli, stop);
        #[cfg(not(all(feature = "cef", target_os = "macos")))]
        return Err("CEF requires the optional macOS spike build: scripts/build-cef.py".into());
    }
    luchs::source::run(cli, stop)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("luchs: {error}");
        std::process::exit(1);
    }
}
