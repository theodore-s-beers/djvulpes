#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery)]

mod cli;
mod commands;

fn main() -> commands::CommandResult<()> {
    cli::run()
}
