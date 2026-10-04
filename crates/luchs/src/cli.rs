use std::{path::PathBuf, process::Command};

use clap::Parser;

use crate::{Result, protocol::validate_size};

#[derive(Clone, Copy, Debug)]
pub struct Size(pub u32, pub u32);

fn parse_size(value: &str) -> std::result::Result<Size, String> {
    let (width, height) = value.split_once('x').ok_or("expected WxH")?;
    let width = width.parse().map_err(|_| "invalid width")?;
    let height = height.parse().map_err(|_| "invalid height")?;
    validate_size(width, height)?;
    Ok(Size(width, height))
}

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Publish a page as an observation-only Jackstay source"
)]
pub struct Cli {
    /// A local HTML file or an http(s) URL.
    pub page: String,
    #[arg(long, default_value = "800x600", value_parser = parse_size)]
    pub size: Size,
    /// Reload a local page when its modification time changes.
    #[arg(long)]
    pub watch: bool,
    /// Name in Jackstay's private per-user runtime directory.
    #[arg(long)]
    pub endpoint: Option<String>,
    /// Renderer executable; defaults to luchs-webview-capture beside luchs.
    #[arg(long)]
    pub helper: Option<PathBuf>,
    /// Compatibility with existing native-WebView launch profiles.
    #[arg(long, default_value = "native-webview", value_parser = ["native-webview"])]
    pub renderer: String,
    /// Stop after this many helper frames (0 means unbounded).
    #[arg(long, default_value_t = 0)]
    pub frames: u32,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=240))]
    pub fps: u32,
    /// Report snapshot, publication and unchanged-frame counters at exit.
    #[arg(long)]
    pub stats: bool,
}

impl Cli {
    pub fn local_page(&self) -> Option<PathBuf> {
        (!self.page.starts_with("http://") && !self.page.starts_with("https://"))
            .then(|| PathBuf::from(&self.page))
    }

    pub fn helper_command(&self) -> Result<Command> {
        let page = match self.local_page() {
            Some(path) => path.canonicalize()?.into_os_string(),
            None => self.page.clone().into(),
        };
        let executable = match &self.helper {
            Some(path) => path.clone(),
            None if cfg!(target_os = "macos") => {
                std::env::current_exe()?.with_file_name("luchs-webview-capture")
            }
            None => {
                return Err(
                    "the WebKit renderer requires macOS; use --helper for a test renderer".into(),
                );
            }
        };
        let mut command = Command::new(executable);
        command.arg(page).args([
            self.size.0.to_string(),
            self.size.1.to_string(),
            self.frames.to_string(),
            self.fps.to_string(),
        ]);
        // Inherit LUCHS_CONSOLE_LOG and the helper's other existing environment options.
        Ok(command)
    }
}
