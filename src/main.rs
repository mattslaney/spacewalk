use anyhow::{Context, Result};
use clap::Parser;
use spacewalk::{
    display_name, human_size,
    scan::{self, Options},
};
use std::{path::PathBuf, sync::atomic::AtomicBool};

#[derive(Parser)]
#[command(
    version,
    about = "Explore disk usage in a linked file tree and treemap"
)]
struct Args {
    /// Directory to scan, including hidden files
    #[arg(default_value = ".")]
    dir: PathBuf,
    /// Use logical file lengths instead of allocated blocks (Unix)
    #[arg(long)]
    apparent_size: bool,
    /// Print a summary without opening the interactive interface
    #[arg(long)]
    summary: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let options = Options {
        apparent_size: args.apparent_size,
    };
    let snapshot = scan::scan(&args.dir, options, &AtomicBool::new(false), |_| {})
        .with_context(|| format!("Cannot scan {}", args.dir.display()))?;
    println!(
        "{}  {}  {} files",
        display_name(snapshot.root.as_os_str()),
        human_size(snapshot.nodes[0].size),
        snapshot.nodes[0].files
    );
    let mut children = snapshot.nodes[0].children.clone();
    children.sort_by_key(|&id| std::cmp::Reverse(snapshot.nodes[id].size));
    for id in children {
        let node = &snapshot.nodes[id];
        println!(
            "{:>12}  {}{}",
            human_size(node.size),
            display_name(&node.name),
            if node.kind == scan::Kind::Directory {
                "/"
            } else {
                ""
            }
        );
    }
    if snapshot.errors > 0 {
        eprintln!("{} scan errors; totals are incomplete", snapshot.errors);
    }
    Ok(())
}
