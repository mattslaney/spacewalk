use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
};
use spacewalk::{
    display_name, human_size,
    scan::{self, Options},
};
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::Duration,
};

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
    if !args.summary {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            bail!(
                "The interactive interface needs a terminal. Use --summary for plain text output."
            );
        }
        let root = args
            .dir
            .canonicalize()
            .with_context(|| format!("Cannot open {}", args.dir.display()))?;
        if !root.is_dir() {
            bail!("Scan target must be a directory: {}", root.display());
        }
        return run_ui(root, options);
    }
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

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        let _ = ratatui::try_restore();
    }
}

fn run_ui(root: PathBuf, options: Options) -> Result<()> {
    let _guard = TerminalGuard;
    let mut terminal = ratatui::try_init().context("Cannot initialize the terminal")?;
    execute!(io::stdout(), EnableMouseCapture)?;
    // Ratatui's panic hook restores the screen; restore mouse capture as well.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        previous_hook(info);
    }));
    let mut app = spacewalk::app::App::new(root, options);
    while !app.quitting {
        app.poll_workers();
        terminal.draw(|frame| spacewalk::ui::draw(frame, &mut app))?;
        if event::poll(Duration::from_millis(80))? {
            match event::read()? {
                Event::Key(key) => app.key(key),
                Event::Mouse(mouse) => app.mouse(mouse),
                _ => {}
            }
        }
    }
    Ok(())
}
