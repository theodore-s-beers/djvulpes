#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery)]

mod cli;
mod commands;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    cli::run()
}
