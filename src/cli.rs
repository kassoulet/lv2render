use clap::Parser;
use std::path::PathBuf;
use crate::plugin::PluginSetting;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Input audio file path
    #[arg(short, long)]
    pub input: PathBuf,

    /// Output processed WAV file path
    #[arg(short, long)]
    pub output: PathBuf,

    /// Effect chain YAML file path
    #[arg(short = 'f', long)]
    pub file: Option<PathBuf>,

    /// List of plugins and parameters (e.g. "plugin_name:param=val")
    #[arg(trailing_var_arg = true, value_parser = crate::config::parse_plugin_setting)]
    pub plugins: Vec<PluginSetting>,

    /// Number of samples per processing cycle
    #[arg(long, default_value_t = 1024)]
    pub block_size: u32,

    /// Seconds of silence to drain after input EOF (for reverb/delay tails)
    #[arg(long, default_value_t = 2.0)]
    pub drain_seconds: f64,
}
