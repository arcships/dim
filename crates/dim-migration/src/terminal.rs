use crate::{Error, Migration, Outcome, Progress, Result, TaskReport};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    execute, queue,
    style::{Color, ResetColor, SetForegroundColor},
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Terminal output for one CLI invocation; ordinary service calls remain headless.
pub struct Terminal {
    interactive: bool,
    json: bool,
}

impl Terminal {
    /// Enables the live view only when both output streams are terminals.
    pub fn new(json: bool) -> Self {
        Self {
            interactive: !json
                && io::stdout().is_terminal()
                && io::stderr().is_terminal()
                && std::env::var_os("TERM").as_deref() != Some(std::ffi::OsStr::new("dumb")),
            json,
        }
    }

    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    pub fn error(&self, home: &Path, error: &str) -> io::Result<()> {
        writeln!(
            io::stderr(),
            "Migration failed: {}\nTarget: {}",
            clip(error, 200),
            clip(&home.to_string_lossy(), 200)
        )
    }

    pub async fn run(
        &self,
        migration: &Arc<Migration>,
        source: PathBuf,
        stopped: bool,
        retry: Option<String>,
    ) -> Result<TaskReport> {
        let target = migration.ledger.home.clone();
        let started = Instant::now();
        let mut screen = Screen::open(self.interactive).map_err(Error::Terminal)?;
        let mut progress = migration.progress();
        let work = migration.run(source.clone(), stopped, retry);
        tokio::pin!(work);
        let mut timer = tokio::time::interval(Duration::from_millis(100));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut output_error = None;
        let mut last_stage = String::new();
        let interrupt = tokio::signal::ctrl_c();
        tokio::pin!(interrupt);
        let report = loop {
            tokio::select! {
                result = &mut work => break result,
                signal = &mut interrupt, if !migration.stopping.is_cancelled() => {
                    if let Err(error) = signal { output_error = Some(error); }
                    migration.cancel_current();
                }
                _ = timer.tick(), if self.interactive && output_error.is_none() => {
                    let snapshot = progress.borrow().clone();
                    let drawing = screen.draw(
                        snapshot.as_ref(),
                        &source,
                        &target,
                        started,
                        migration.stopping.is_cancelled(),
                    );
                    if let Err(error) = drawing {
                        output_error = Some(error);
                        migration.cancel_current();
                    }
                }
                changed = progress.changed(),
                    if !self.interactive && !self.json && output_error.is_none() =>
                {
                    let next_stage = progress
                        .borrow()
                        .as_ref()
                        .map(|progress| progress.task.stage.clone());
                    if changed.is_ok() && let Some(next_stage) = next_stage
                        && next_stage != last_stage
                    {
                        last_stage = next_stage;
                        if let Err(error) = writeln!(io::stderr(), "Migration: {}", stage(&last_stage).1) {
                            output_error = Some(error);
                            migration.cancel_current();
                        }
                    }
                }
            }
        };
        screen.close().map_err(Error::Terminal)?;
        if let Some(error) = output_error {
            return Err(Error::Terminal(error));
        }
        let report = report?;
        if self.interactive {
            summary(&report, &target).map_err(Error::Terminal)?;
        }
        Ok(report)
    }
}

struct Screen {
    active: bool,
}

impl Screen {
    fn open(active: bool) -> io::Result<Self> {
        let screen = Self { active };
        if active {
            execute!(io::stderr(), EnterAlternateScreen, Hide)?;
        }
        Ok(screen)
    }

    fn close(&mut self) -> io::Result<()> {
        if self.active {
            execute!(io::stderr(), ResetColor, Show, LeaveAlternateScreen)?;
            self.active = false;
        }
        Ok(())
    }

    fn draw(
        &self,
        progress: Option<&Progress>,
        source: &Path,
        target: &Path,
        started: Instant,
        stopping: bool,
    ) -> io::Result<()> {
        let (columns, rows) = crossterm::terminal::size()?;
        let width = usize::from(columns.saturating_sub(6)).min(96);
        let elapsed = started.elapsed().as_secs();
        let spinner = ["|", "/", "-", "\\"][(started.elapsed().as_millis() / 100 % 4) as usize];
        let status = if stopping { "STOPPING" } else { "RUNNING" };
        let (number, name) = progress
            .map(|p| stage(&p.task.stage))
            .unwrap_or((0, "Preparing"));
        let count = match progress.and_then(|p| p.task.total.map(|total| (p.task.processed, total)))
        {
            Some((done, total)) => format!("{done}/{total}"),
            None => format!("{spinner} In progress"),
        };
        let mut output = io::BufWriter::new(io::stderr().lock());
        queue!(output, MoveTo(0, 0), Clear(ClearType::All))?;
        if columns < 40 || rows < 20 {
            writeln!(
                output,
                "{}\r",
                clip(
                    &format!("Dim Migration [{status}] {name} {count}"),
                    usize::from(columns.saturating_sub(1))
                )
            )?;
            if rows > 2 {
                writeln!(
                    output,
                    "{}\r",
                    clip(
                        "Ctrl+C: stop after accepted writes settle",
                        usize::from(columns.saturating_sub(1))
                    )
                )?;
            }
            return output.flush();
        }
        let mut lines = vec![(format!("┌{}┐", "─".repeat(width + 2)), None)];
        let mut row = |text: String, color: Option<Color>| {
            let text = clip(&text, width);
            let padding = width.saturating_sub(text.width());
            lines.push((format!("│ {text}{} │", " ".repeat(padding)), color));
        };
        row(
            format!("Dim / Migration  {spinner}  {status}"),
            Some(Color::Magenta),
        );
        row(String::new(), None);
        row(format!("Source  {}", source.display()), None);
        row(format!("Target  {}", target.display()), None);
        row(String::new(), None);
        row(
            if number == 0 {
                name.to_owned()
            } else {
                format!("Step {number}/7  {name}")
            },
            Some(Color::Magenta),
        );
        let current = progress.and_then(|progress| progress.task.current.as_deref());
        row(
            match current {
                Some(current) => format!("Current step  {count}    {}", clip(current, width / 2)),
                None => format!("Current step  {count}"),
            },
            None,
        );
        if let Some((done, total)) =
            progress.and_then(|p| p.task.total.map(|total| (p.task.processed, total)))
        {
            let percent = if total == 0 {
                0.0
            } else {
                done.min(total) as f64 / total as f64
            };
            let bars = width.saturating_sub(10);
            let filled = (percent * bars as f64) as usize;
            row(
                format!(
                    "[{}{}] {:5.1}%",
                    "#".repeat(filled),
                    "-".repeat(bars - filled),
                    percent * 100.0
                ),
                Some(Color::Magenta),
            );
        } else {
            row(format!("{spinner} Waiting for a known object count"), None);
        }
        row(String::new(), None);
        let limit = if columns >= 80 && rows >= 24 { 8 } else { 5 };
        row(format!("Recent results (up to {limit})"), None);
        let recent = progress.map(|p| &p.recent);
        let length = recent.map_or(0, |recent| recent.len().min(limit));
        for _ in length..limit {
            row(String::new(), None);
        }
        if let Some(recent) = recent {
            for result in recent.iter().skip(recent.len().saturating_sub(limit)) {
                let detail = result.reason.as_deref().unwrap_or("");
                row(
                    format!(
                        "{:<17} {}  {}",
                        outcome(result.outcome).0,
                        clip(&result.id, width / 3),
                        detail
                    ),
                    Some(outcome(result.outcome).1),
                );
            }
        }
        row(String::new(), None);
        row(
            format!(
                "Elapsed {:02}:{:02}  Task {}",
                elapsed / 60,
                elapsed % 60,
                progress
                    .map(|progress| clip(&progress.task.id, 12))
                    .unwrap_or_default()
            ),
            None,
        );
        row(
            if stopping {
                "Stopping; waiting for accepted writes to settle".into()
            } else {
                "Ctrl+C: stop after accepted writes settle".into()
            },
            None,
        );
        lines.push((format!("└{}┘", "─".repeat(width + 2)), None));
        for (index, (line, color)) in lines.into_iter().enumerate() {
            queue!(output, MoveTo(1, u16::try_from(index).unwrap()))?;
            if std::env::var_os("NO_COLOR").is_none()
                && let Some(color) = color
            {
                queue!(output, SetForegroundColor(color))?;
            }
            write!(output, "{line}")?;
            queue!(output, ResetColor)?;
        }
        output.flush()
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn stage(value: &str) -> (u8, &str) {
    match value {
        "scan" => (1, "Scan source"),
        "providers" => (2, "Import providers"),
        "settings" => (3, "Import settings"),
        "accounts" => (4, "Import accounts"),
        "sessions" => (5, "Import sessions"),
        "scheduler" => (6, "Import schedules"),
        "inventory" => (7, "Check retained data"),
        "complete" => (7, "Complete"),
        _ => (0, value),
    }
}

fn outcome(value: Outcome) -> (&'static str, Color) {
    match value {
        Outcome::Imported => ("Imported", Color::Green),
        Outcome::AlreadyImported => ("Already imported", Color::Grey),
        Outcome::Deleted => ("Deleted", Color::Grey),
        Outcome::Conflict => ("Conflict", Color::Yellow),
        Outcome::Deferred => ("Deferred", Color::Yellow),
        Outcome::Unsupported => ("Unsupported", Color::Yellow),
        Outcome::Failed => ("Failed", Color::Red),
        Outcome::Cancelled => ("Cancelled", Color::Yellow),
    }
}

fn clip(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for character in text.chars().take(width.saturating_mul(4)) {
        if character.is_control() {
            continue;
        }
        let size = character.width().unwrap_or(0);
        if used + size > width {
            while result.width() >= width {
                result.pop();
            }
            if width > 0 {
                result.push('…');
            }
            break;
        }
        result.push(character);
        used += size;
    }
    result
}

fn quote(value: &str) -> String {
    let value: String = value
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    if cfg!(windows) {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn summary(report: &TaskReport, home: &Path) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(
        output,
        "Migration {:?}\nTarget: {}\nTask: {}",
        report.task.status,
        clip(&home.to_string_lossy(), 160),
        report.task.id
    )?;
    for value in [
        Outcome::Imported,
        Outcome::AlreadyImported,
        Outcome::Deleted,
        Outcome::Conflict,
        Outcome::Deferred,
        Outcome::Unsupported,
        Outcome::Failed,
        Outcome::Cancelled,
    ] {
        let count = report
            .objects
            .iter()
            .filter(|result| result.outcome == value)
            .count();
        if count != 0 {
            writeln!(output, "  {}: {count}", outcome(value).0)?;
        }
    }
    let omitted: usize = report
        .objects
        .iter()
        .map(|result| result.omitted.len())
        .sum();
    if omitted != 0 {
        writeln!(output, "  Reported omissions: {omitted}")?;
    }
    if let Some(error) = &report.task.error {
        writeln!(output, "{}", clip(error, 200))?;
    }
    let program = std::env::current_exe()?;
    writeln!(
        output,
        "Full report: {}{} migration show {} --home {}",
        if cfg!(windows) { "& " } else { "" },
        quote(&program.to_string_lossy()),
        report.task.id,
        quote(&home.to_string_lossy())
    )?;
    output.flush()
}
