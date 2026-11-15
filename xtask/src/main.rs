//! Workspace build tasks: `cargo xtask man <DIR>` writes zkctl man pages,
//! `cargo xtask completions <DIR>` writes shell completions, both from the
//! command-line definition in `zkctl::cli`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use zkctl::cli::Cli;

#[derive(Parser)]
#[command(about = "Ferrite build tasks")]
struct Args {
    #[command(subcommand)]
    task: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Write zkctl.1 and one page per subcommand.
    Man {
        /// Output directory.
        dir: PathBuf,
    },
    /// Write completion scripts for every supported shell.
    Completions {
        /// Output directory.
        dir: PathBuf,
    },
}

fn main() -> Result<()> {
    match Args::parse().task {
        Task::Man { dir } => man(&dir),
        Task::Completions { dir } => completions(&dir),
    }
}

fn man(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    clap_mangen::generate_to(Cli::command(), dir).context("writing man pages")?;
    eprintln!("wrote man pages to {}", dir.display());
    Ok(())
}

fn completions(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for shell in [
        Shell::Bash,
        Shell::Zsh,
        Shell::Fish,
        Shell::PowerShell,
        Shell::Elvish,
    ] {
        let path = clap_complete::generate_to(shell, &mut Cli::command(), "zkctl", dir)
            .with_context(|| format!("writing {shell} completions"))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}
