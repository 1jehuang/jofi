use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use jofi::desktop::{DiscoveryOptions, discover_desktop_entries};
use jofi::history::History;
use jofi::launcher::{build_launch_command, launch};
use jofi::search::{SearchIndex, SearchResult};
use jofi::telemetry::{MemorySnapshot, Telemetry, default_telemetry_path};
use jofi::ui::{UiOptions, run_launcher};
use serde::Serialize;
use serde_json::{Map, json};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    /// Disable telemetry logging for this invocation.
    #[arg(long, global = true)]
    no_telemetry: bool,

    /// Telemetry JSONL file path. Defaults to $XDG_STATE_HOME/jofi/telemetry.jsonl.
    #[arg(long, global = true)]
    telemetry_log: Option<PathBuf>,

    /// Include NoDisplay=true desktop entries.
    #[arg(long, global = true)]
    include_hidden: bool,

    #[command(flatten)]
    ui: UiArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Args, Clone)]
struct UiArgs {
    /// Font file to use for the Wayland UI. Also configurable with JOFI_FONT.
    #[arg(long)]
    font: Option<PathBuf>,

    /// Fullscreen background opacity from 0-255. Defaults to tofi's #000A alpha.
    #[arg(long, default_value_t = 170)]
    background_alpha: u8,

    /// Query text size in physical pixels.
    #[arg(long, default_value_t = 34.0)]
    query_size: f32,

    /// Result text size in physical pixels.
    #[arg(long, default_value_t = 28.0)]
    result_size: f32,

    /// Vertical gap between result entries in physical pixels.
    #[arg(long, default_value_t = 25.0)]
    result_gap: f32,

    /// Horizontal text start position as a fraction of screen width.
    #[arg(long, default_value_t = 0.35)]
    x_percent: f32,

    /// Vertical text start position as a fraction of screen height.
    #[arg(long, default_value_t = 0.35)]
    y_percent: f32,

    /// Integer Wayland buffer scale. Higher values render sharper text at the cost of draw time.
    #[arg(long, default_value_t = 3)]
    render_scale: u32,

    /// Maximum visible launcher results.
    #[arg(long, default_value_t = 5)]
    ui_results: usize,
}

impl From<UiArgs> for UiOptions {
    fn from(args: UiArgs) -> Self {
        UiOptions {
            font_path: args.font,
            background_alpha: args.background_alpha,
            query_size_px: args.query_size,
            result_size_px: args.result_size,
            result_gap_px: args.result_gap,
            x_percent: args.x_percent,
            y_percent: args.y_percent,
            render_scale: args.render_scale.max(1),
            max_results: args.ui_results,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open the graphical Wayland launcher UI.
    Run(UiArgs),
    /// List indexed desktop entries.
    List(OutputArgs),
    /// Search desktop entries with jofi's typo-resistant ranker.
    Search(SearchArgs),
    /// Launch the best matching desktop entry.
    Launch(LaunchArgs),
    /// Run indexing/search performance probes and print timing + memory metrics.
    Profile(ProfileArgs),
}

#[derive(Debug, Args)]
struct OutputArgs {
    /// Output JSON instead of a compact text table.
    #[arg(long)]
    json: bool,

    /// Maximum entries to print.
    #[arg(long, default_value_t = 100)]
    limit: usize,
}

#[derive(Debug, Args)]
struct SearchArgs {
    /// Search query.
    query: String,

    /// Maximum results to print.
    #[arg(short, long, default_value_t = 10)]
    limit: usize,

    /// Output JSON instead of a compact text table.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct LaunchArgs {
    /// Search query. The highest-ranked result is launched.
    query: String,

    /// Print the launch command instead of executing it.
    #[arg(long)]
    dry_run: bool,

    /// Output JSON for dry-run/selection details.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ProfileArgs {
    /// Queries to benchmark. Can be supplied more than once.
    #[arg(short, long)]
    query: Vec<String>,

    /// Number of timed search repetitions per query.
    #[arg(short, long, default_value_t = 200)]
    runs: usize,

    /// Maximum search results produced per run.
    #[arg(short, long, default_value_t = 10)]
    limit: usize,

    /// Output JSON instead of human-readable text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct ProfileReport {
    entries: usize,
    discovery_ns: u128,
    index_build_ns: u128,
    search_reports: Vec<QueryProfile>,
    memory: MemorySnapshot,
    telemetry_log: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct QueryProfile {
    query: String,
    runs: usize,
    avg_ns: f64,
    min_ns: u128,
    max_ns: u128,
    result_count: usize,
    best_match: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let telemetry_path =
        (!cli.no_telemetry).then(|| cli.telemetry_log.unwrap_or_else(default_telemetry_path));
    let telemetry = Telemetry::new(telemetry_path)?;
    let discovery_options = DiscoveryOptions {
        include_hidden: cli.include_hidden,
    };

    match cli.command {
        Some(Command::Run(args)) => cmd_run(args, &discovery_options, telemetry),
        Some(Command::List(args)) => cmd_list(args, &discovery_options, &telemetry),
        Some(Command::Search(args)) => cmd_search(args, &discovery_options, &telemetry),
        Some(Command::Launch(args)) => cmd_launch(args, &discovery_options, &telemetry),
        Some(Command::Profile(args)) => cmd_profile(args, &discovery_options, &telemetry),
        None => cmd_run(cli.ui, &discovery_options, telemetry),
    }
}

fn cmd_run(args: UiArgs, options: &DiscoveryOptions, telemetry: Telemetry) -> Result<()> {
    let history = load_history(&telemetry)?;
    let index = load_index_with_history(options, &telemetry, &history)?;
    run_launcher(index, telemetry, args.into(), history)
}

fn load_history(telemetry: &Telemetry) -> Result<History> {
    let mut span = telemetry.span("history.load");
    let history = History::load_with_tofi_fallback()?;
    span.set_field("entries", history.len());
    Ok(history)
}

fn load_index(options: &DiscoveryOptions, telemetry: &Telemetry) -> Result<SearchIndex> {
    let history = load_history(telemetry)?;
    load_index_with_history(options, telemetry, &history)
}

fn load_index_with_history(
    options: &DiscoveryOptions,
    telemetry: &Telemetry,
    history: &History,
) -> Result<SearchIndex> {
    let entries = {
        let mut span = telemetry.span("desktop.discover");
        let entries = discover_desktop_entries(options)?;
        span.set_field("entries", entries.len());
        entries
    };

    let index = {
        let mut span = telemetry.span("search.index_build");
        let index = SearchIndex::with_history(entries, history);
        span.set_field("entries", index.len());
        index
    };
    Ok(index)
}

fn cmd_list(args: OutputArgs, options: &DiscoveryOptions, telemetry: &Telemetry) -> Result<()> {
    let index = load_index(options, telemetry)?;
    let entries = index.entries().take(args.limit).collect::<Vec<_>>();
    if args.json {
        serde_json::to_writer_pretty(std::io::stdout(), &entries)?;
        println!();
        return Ok(());
    }

    println!("{:<42} {:<8} Exec", "Name", "Terminal");
    println!("{:-<42} {:-<8} {:-<30}", "", "", "");
    for entry in entries {
        println!(
            "{:<42} {:<8} {}",
            truncate(&entry.name, 42),
            entry.terminal,
            truncate(&entry.exec, 90)
        );
    }
    Ok(())
}

fn cmd_search(args: SearchArgs, options: &DiscoveryOptions, telemetry: &Telemetry) -> Result<()> {
    let index = load_index(options, telemetry)?;
    let results = {
        let mut span = telemetry.span("search.query").field("query", &args.query);
        let results = index.search(&args.query, args.limit);
        span.set_field("results", results.len());
        results
    };

    if args.json {
        serde_json::to_writer_pretty(std::io::stdout(), &results)?;
        println!();
        return Ok(());
    }
    print_results(&results);
    Ok(())
}

fn cmd_launch(args: LaunchArgs, options: &DiscoveryOptions, telemetry: &Telemetry) -> Result<()> {
    let index = load_index(options, telemetry)?;
    let mut results = {
        let mut span = telemetry.span("search.query").field("query", &args.query);
        let results = index.search(&args.query, 1);
        span.set_field("results", results.len());
        results
    };
    let result = results
        .pop()
        .with_context(|| format!("no desktop entry matched query {:?}", args.query))?;
    let command = build_launch_command(&result.entry)?;

    if args.dry_run {
        if args.json {
            let payload = json!({
                "selected": result,
                "command": command,
                "telemetry_log": telemetry.path(),
            });
            serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
            println!();
        } else {
            println!("selected: {}", result.entry.name);
            println!("score: {}", result.score);
            println!("command: {}", shell_words::join(command.as_vec()));
        }
        return Ok(());
    }

    let mut fields = Map::new();
    fields.insert("entry".to_string(), json!(result.entry.name));
    fields.insert("query".to_string(), json!(args.query));
    fields.insert("command".to_string(), json!(command.as_vec()));
    telemetry.event("launch.selected", fields);

    let mut span = telemetry
        .span("launch.spawn")
        .field("entry", &result.entry.name);
    let child = launch(&result.entry)?;
    let mut history = History::load_with_tofi_fallback()?;
    history.increment(&result.entry.name);
    if let Err(err) = history.save() {
        let mut fields = Map::new();
        fields.insert("entry".to_string(), json!(result.entry.name));
        fields.insert("error".to_string(), json!(err.to_string()));
        telemetry.event("history.save_error", fields);
    }
    span.set_field("child_pid", child.id());
    drop(child);
    Ok(())
}

fn cmd_profile(args: ProfileArgs, options: &DiscoveryOptions, telemetry: &Telemetry) -> Result<()> {
    if args.runs == 0 {
        bail!("--runs must be greater than zero");
    }

    let discovery_start = Instant::now();
    let entries = {
        let mut span = telemetry.span("profile.desktop.discover");
        let entries = discover_desktop_entries(options)?;
        span.set_field("entries", entries.len());
        entries
    };
    let discovery_ns = discovery_start.elapsed().as_nanos();

    let index_start = Instant::now();
    let index = {
        let mut span = telemetry.span("profile.search.index_build");
        let index = SearchIndex::new(entries);
        span.set_field("entries", index.len());
        index
    };
    let index_build_ns = index_start.elapsed().as_nanos();

    let queries = if args.query.is_empty() {
        vec![
            "firefox".to_string(),
            "chrmoe".to_string(),
            "hotspot".to_string(),
        ]
    } else {
        args.query
    };

    let mut search_reports = Vec::new();
    for query in queries {
        let mut total = 0_u128;
        let mut min = u128::MAX;
        let mut max = 0_u128;
        let mut last_results = Vec::new();
        {
            let mut span = telemetry
                .span("profile.search.query")
                .field("query", &query)
                .field("runs", args.runs);
            for _ in 0..args.runs {
                let start = Instant::now();
                last_results = index.search(&query, args.limit);
                let elapsed = start.elapsed().as_nanos();
                total += elapsed;
                min = min.min(elapsed);
                max = max.max(elapsed);
            }
            span.set_field("results_last_run", last_results.len());
            span.set_field("avg_ns", total as f64 / args.runs as f64);
        }
        search_reports.push(QueryProfile {
            query,
            runs: args.runs,
            avg_ns: total as f64 / args.runs as f64,
            min_ns: min,
            max_ns: max,
            result_count: last_results.len(),
            best_match: last_results.first().map(|r| r.entry.name.clone()),
        });
    }

    let report = ProfileReport {
        entries: index.len(),
        discovery_ns,
        index_build_ns,
        search_reports,
        memory: MemorySnapshot::current(),
        telemetry_log: telemetry.path().map(PathBuf::from),
    };

    if args.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        print_profile_report(&report);
    }
    Ok(())
}

fn print_results(results: &[SearchResult]) {
    println!("{:<8} {:<34} {:<24} Reason", "Score", "Name", "ID");
    println!("{:-<8} {:-<34} {:-<24} {:-<30}", "", "", "", "");
    for result in results {
        println!(
            "{:<8} {:<34} {:<24} {}",
            result.score,
            truncate(&result.entry.name, 34),
            truncate(&result.entry.id, 24),
            truncate(&result.reason, 80),
        );
    }
}

fn print_profile_report(report: &ProfileReport) {
    println!("entries: {}", report.entries);
    println!(
        "discovery: {:.3} ms",
        report.discovery_ns as f64 / 1_000_000.0
    );
    println!(
        "index_build: {:.3} ms",
        report.index_build_ns as f64 / 1_000_000.0
    );
    println!(
        "memory: rss={:?} KiB vm_size={:?} KiB",
        report.memory.rss_kib, report.memory.vm_size_kib
    );
    if let Some(path) = &report.telemetry_log {
        println!("telemetry_log: {}", path.display());
    }
    println!();
    println!(
        "{:<18} {:>12} {:>12} {:>12} {:>8} Best match",
        "Query", "Avg ns", "Min ns", "Max ns", "Results"
    );
    println!(
        "{:-<18} {:-<12} {:-<12} {:-<12} {:-<8} {:-<30}",
        "", "", "", "", "", ""
    );
    for query in &report.search_reports {
        println!(
            "{:<18} {:>12.0} {:>12} {:>12} {:>8} {}",
            truncate(&query.query, 18),
            query.avg_ns,
            query.min_ns,
            query.max_ns,
            query.result_count,
            query.best_match.as_deref().unwrap_or("-"),
        );
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out = s
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}
