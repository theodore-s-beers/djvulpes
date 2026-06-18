#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery)]

mod cli;
mod commands;

fn main() -> anyhow::Result<()> {
    cli::run()
}
