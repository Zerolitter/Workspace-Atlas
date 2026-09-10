//! Workspace Atlas CLI binary.

use clap::Parser;

use workspace_atlas::cli::{run, Cli};

fn main() {
    let cli = Cli::parse();
    run(cli);
}
