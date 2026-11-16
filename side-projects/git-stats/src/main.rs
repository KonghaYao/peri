mod analysis;
mod commit;
mod git;
mod render;

use chrono::{Duration, Local, NaiveDate};
use clap::Parser;

/// Analyze git commit activity in a time window.
/// Shows per-person stats including commits, lines changed,
/// commit type distribution, and Co-Authored-By relationships.
#[derive(Parser, Debug)]
#[command(name = "git-stats", version)]
struct Cli {
    /// Number of days to look back (default: 7)
    #[arg(long, default_value = "7", conflicts_with_all = ["since", "until"])]
    days: u32,

    /// Start date (YYYY-MM-DD), exclusive with --days
    #[arg(long, conflicts_with = "days")]
    since: Option<String>,

    /// End date (YYYY-MM-DD), exclusive with --days
    #[arg(long, conflicts_with = "days")]
    until: Option<String>,

    /// Git repository path (default: current directory)
    #[arg(long, default_value = ".")]
    repo: String,
}

fn resolve_dates(cli: &Cli) -> (String, String) {
    let today = Local::now().date_naive();
    let until = match &cli.until {
        Some(d) => NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .expect("Invalid --until date format, expected YYYY-MM-DD"),
        None => today.succ_opt().unwrap_or(today),
    };
    let since = match &cli.since {
        Some(d) => NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .expect("Invalid --since date format, expected YYYY-MM-DD"),
        None => today - Duration::days(cli.days as i64),
    };
    (
        since.format("%Y-%m-%d").to_string(),
        until.format("%Y-%m-%d").to_string(),
    )
}

fn main() {
    let cli = Cli::parse();
    let (since, until) = resolve_dates(&cli);

    eprintln!(
        "Analyzing commits from {} to {} in {}...",
        since, until, cli.repo
    );

    let commits = git::fetch_commits(&cli.repo, &since, &until).unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    });

    let stats = analysis::aggregate(&commits);
    render::render_table(&stats);
}
