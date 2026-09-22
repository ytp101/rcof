use anyhow::Result;
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
#[derive(Parser)]
#[command(version, about = "A local Rust audio/video conferencing learning lab")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Run a configured loopback UDP impairment relay.
    Link(rcof::link::Options),
    /// Run the loopback-only room/signaling service.
    Server {
        #[arg(long, default_value = "127.0.0.1:8790")]
        listen: SocketAddr,
    },
    /// List audio devices and their default formats.
    Devices,
    /// Join a local two-person audio/video call. Defaults to generated tone + silent decoder.
    Call(rcof::call::Options),
    /// Open the native audio/video call window.
    Gui(rcof::ui::Options),
}
fn main() -> Result<()> {
    let command = Cli::parse()
        .command
        .unwrap_or_else(|| Command::Gui(rcof::ui::Options::default()));
    if let Command::Gui(options) = command {
        return rcof::ui::run(options);
    }
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(async {
            match command {
                Command::Server { listen } => rcof::signaling::serve(listen).await,
                Command::Link(options) => rcof::link::run(options).await,
                Command::Devices => rcof::audio::devices(),
                Command::Call(options) => rcof::call::run(options).await,
                Command::Gui(_) => unreachable!(),
            }
        })
}
