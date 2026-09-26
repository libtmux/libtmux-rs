//! Native tmuxp-compatible workspace command-line application.

mod cli;

fn main() -> std::process::ExitCode {
    cli::main()
}
