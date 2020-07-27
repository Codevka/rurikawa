use clap::Clap;
use std::path::PathBuf;

#[derive(Clap, Debug, Clone)]
pub struct Opts {
    #[clap(subcommand)]
    cmd: SubCmd,
}

#[derive(Clap, Debug, Clone)]
pub enum SubCmd {
    /// Run as a long-running runner instance
    #[clap(name = "server")]
    Server(ServerSubCmd),

    /// Run a single test job in local environment
    #[clap(name = "run")]
    Run(RunSubCmd),
}

#[derive(Clap, Debug, Clone)]
pub struct ServerSubCmd {
    /// The coordinator's uri (include port if needed)
    #[clap(required = true)]
    host: String,

    /// Access token
    #[clap(long, short)]
    token: Option<String>,
}

#[derive(Clap, Debug, Clone)]
pub struct RunSubCmd {
    /// The job to run. Either specify a folder where `judge.toml` can be found
    /// in it or its subfolders, or specify a file to be used as `judge.toml`.
    /// Defaults to current folder.
    #[clap(name = "job-path")]
    job: Option<PathBuf>,

    /// Configuration file of tests.
    #[clap(long, short, name = "config-file-path")]
    config: Option<PathBuf>,
}
