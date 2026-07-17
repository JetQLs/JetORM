//! `jet ui` — the interactive migration dashboard.
//!
//! The terminal loop is a thin shell around a pure state machine: every
//! keypress maps to an [`Action`], every action to a state transition, and
//! the transitions are unit-tested without a terminal. Database effects run
//! between transitions, never inside them.
//!
//! The dashboard shows every migration with its state, the selected
//! migration's steps, and — when a target schema is given — the drift
//! between the migration files and the target. Destructive steps are
//! marked, applying demands an explicit confirmation, and rename
//! candidates are confirmed interactively: the differ never guesses, and
//! here saying yes actually rewrites the plan through `confirm_rename`.

use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use jetorm_executor::Database;
use jetorm_migration::{MigrationSet, MigrationState, Migrator};
use jetorm_schema::{RenameCandidate, SchemaDiff, SchemaSet, diff};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

/// One migration row of the dashboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationRow {
    /// Version identifier.
    pub version: String,
    /// Whether the database has it applied.
    pub applied: bool,
    /// Whether any up step can lose data or fail on populated tables.
    pub destructive: bool,
    /// Rendered up-step descriptions.
    pub steps: Vec<String>,
}

/// What the dashboard is currently asking of the operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Browsing the migration list.
    Browse,
    /// Confirming application of the next pending migrations.
    ConfirmApply {
        /// Versions that would apply, in order.
        versions: Vec<String>,
        /// Whether any of them is destructive.
        destructive: bool,
    },
    /// Confirming one rename candidate of the pending drift.
    ConfirmRename {
        /// Remaining candidates, first one being asked about.
        candidates: Vec<RenameCandidate>,
    },
    /// Showing a completed action's outcome until any key.
    Notice(String),
}

/// The dashboard's whole state, transitioned purely by [`Ui::apply_action`].
#[derive(Clone, Debug)]
pub struct Ui {
    /// Migration rows in application order.
    pub rows: Vec<MigrationRow>,
    /// Selected row index.
    pub selected: usize,
    /// Drift between migration files and the target schema, when given.
    pub drift: Vec<String>,
    /// Unconfirmed rename candidates of the current drift.
    pub candidates: Vec<RenameCandidate>,
    /// Current interaction mode.
    pub mode: Mode,
    /// Whether the operator asked to leave.
    pub quit: bool,
}

/// One semantic input, decoded from a key event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    Refresh,
    StartApply,
    StartRenameReview,
    Confirm,
    Cancel,
    Quit,
}

/// What the shell must do after a transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Nothing; keep drawing.
    None,
    /// Re-read migrations, history, and drift.
    Reload,
    /// Apply exactly these pending versions.
    Apply(Vec<String>),
    /// Rewrite the drift through one confirmed rename.
    ConfirmRename(RenameCandidate),
}

impl Ui {
    /// Builds the dashboard state from loaded data.
    #[must_use]
    pub fn new(
        rows: Vec<MigrationRow>,
        drift: Vec<String>,
        candidates: Vec<RenameCandidate>,
    ) -> Self {
        Self {
            rows,
            selected: 0,
            drift,
            candidates,
            mode: Mode::Browse,
            quit: false,
        }
    }

    /// Decodes one key press under the current mode.
    #[must_use]
    pub fn action_of(&self, code: KeyCode) -> Option<Action> {
        match (&self.mode, code) {
            (Mode::Browse, KeyCode::Up | KeyCode::Char('k')) => Some(Action::Up),
            (Mode::Browse, KeyCode::Down | KeyCode::Char('j')) => Some(Action::Down),
            (Mode::Browse, KeyCode::Char('r')) => Some(Action::Refresh),
            (Mode::Browse, KeyCode::Char('u')) => Some(Action::StartApply),
            (Mode::Browse, KeyCode::Char('n')) => Some(Action::StartRenameReview),
            (Mode::Browse, KeyCode::Char('q') | KeyCode::Esc) => Some(Action::Quit),
            (Mode::Notice(_), _) => Some(Action::Cancel),
            (_, KeyCode::Char('y') | KeyCode::Enter) => Some(Action::Confirm),
            (_, KeyCode::Char('n' | 'q') | KeyCode::Esc) => Some(Action::Cancel),
            _ => None,
        }
    }

    /// Transitions the state and names the effect the shell must run.
    pub fn apply_action(&mut self, action: Action) -> Effect {
        match (&mut self.mode, action) {
            (Mode::Browse, Action::Up) => {
                self.selected = self.selected.saturating_sub(1);
                Effect::None
            }
            (Mode::Browse, Action::Down) => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                Effect::None
            }
            (Mode::Browse, Action::Refresh) => Effect::Reload,
            (Mode::Browse, Action::StartApply) => {
                let pending: Vec<&MigrationRow> =
                    self.rows.iter().filter(|row| !row.applied).collect();
                if pending.is_empty() {
                    self.mode = Mode::Notice("nothing to apply".to_owned());
                    return Effect::None;
                }
                self.mode = Mode::ConfirmApply {
                    versions: pending.iter().map(|row| row.version.clone()).collect(),
                    destructive: pending.iter().any(|row| row.destructive),
                };
                Effect::None
            }
            (Mode::Browse, Action::StartRenameReview) => {
                if self.candidates.is_empty() {
                    self.mode = Mode::Notice("no rename candidates".to_owned());
                    return Effect::None;
                }
                self.mode = Mode::ConfirmRename {
                    candidates: self.candidates.clone(),
                };
                Effect::None
            }
            (Mode::Browse | Mode::Notice(_), Action::Quit) => {
                self.quit = true;
                Effect::None
            }
            (Mode::ConfirmApply { versions, .. }, Action::Confirm) => {
                let versions = versions.clone();
                self.mode = Mode::Browse;
                Effect::Apply(versions)
            }
            (Mode::ConfirmRename { candidates }, Action::Confirm) => {
                let confirmed = candidates.remove(0);
                if candidates.is_empty() {
                    self.mode = Mode::Browse;
                }
                Effect::ConfirmRename(confirmed)
            }
            (Mode::ConfirmRename { candidates }, Action::Cancel) => {
                // Declining one candidate moves on to the next; the
                // drop-plus-add pair simply stays in the plan.
                candidates.remove(0);
                if candidates.is_empty() {
                    self.mode = Mode::Browse;
                }
                Effect::None
            }
            (_, Action::Cancel) => {
                self.mode = Mode::Browse;
                Effect::None
            }
            _ => Effect::None,
        }
    }
}

/// Everything the dashboard shows, reloaded on demand.
struct Snapshot {
    rows: Vec<MigrationRow>,
    drift: Vec<String>,
    candidates: Vec<RenameCandidate>,
    /// The live diff object renames rewrite; kept so confirmations apply.
    diff: Option<SchemaDiff>,
}

async fn load(
    database: &Database,
    dir: &Path,
    schema: Option<&SchemaSet>,
) -> Result<Snapshot, String> {
    let migrations = MigrationSet::from_directory(dir).map_err(|error| error.to_string())?;
    let migrator = Migrator::new(database, &migrations);
    migrator
        .install()
        .await
        .map_err(|error| error.to_string())?;
    let statuses = migrator.status().await.map_err(|error| error.to_string())?;
    let rows = statuses
        .iter()
        .filter_map(|status| {
            let migration = migrations.get(status.version())?;
            Some(MigrationRow {
                version: status.version().to_owned(),
                applied: status.state() == MigrationState::Applied,
                destructive: migration.is_destructive(),
                steps: migration
                    .up()
                    .iter()
                    .flat_map(|step| step.state_changes())
                    .map(ToString::to_string)
                    .collect(),
            })
        })
        .collect();

    let (drift, candidates, live_diff) = match schema {
        Some(target) => {
            let replayed = migrations.replay().map_err(|error| error.to_string())?;
            let changes = diff(&replayed, target);
            (
                changes.changes().iter().map(ToString::to_string).collect(),
                changes.rename_candidates().to_vec(),
                Some(changes),
            )
        }
        None => (Vec::new(), Vec::new(), None),
    };
    Ok(Snapshot {
        rows,
        drift,
        candidates,
        diff: live_diff,
    })
}

/// Runs the dashboard until the operator quits.
///
/// # Errors
///
/// Returns an error when the terminal, the migration files, or the
/// database cannot be used.
pub async fn run(
    database: &Database,
    dir: PathBuf,
    schema: Option<SchemaSet>,
) -> Result<(), String> {
    let mut snapshot = load(database, &dir, schema.as_ref()).await?;
    let mut ui = Ui::new(
        snapshot.rows.clone(),
        snapshot.drift.clone(),
        snapshot.candidates.clone(),
    );

    let mut terminal = ratatui::try_init().map_err(|error| error.to_string())?;
    let outcome = loop {
        if let Err(error) = terminal.draw(|frame| draw(frame, &ui)) {
            break Err(error.to_string());
        }
        let Ok(event) = event::read() else {
            break Err("terminal input closed".to_owned());
        };
        let Event::Key(key) = event else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let Some(action) = ui.action_of(key.code) else {
            continue;
        };
        let effect = ui.apply_action(action);
        if ui.quit {
            break Ok(());
        }
        let result = run_effect(effect, database, &dir, schema.as_ref(), &mut snapshot).await;
        match result {
            Ok(Some(notice)) => {
                ui = Ui::new(
                    snapshot.rows.clone(),
                    snapshot.drift.clone(),
                    snapshot.candidates.clone(),
                );
                ui.mode = Mode::Notice(notice);
            }
            Ok(None) => {}
            Err(error) => {
                ui.mode = Mode::Notice(format!("error: {error}"));
            }
        }
    };
    ratatui::restore();
    outcome
}

/// Runs one effect; `Some` notice means state was reloaded.
async fn run_effect(
    effect: Effect,
    database: &Database,
    dir: &Path,
    schema: Option<&SchemaSet>,
    snapshot: &mut Snapshot,
) -> Result<Option<String>, String> {
    match effect {
        Effect::None => Ok(None),
        Effect::Reload => {
            *snapshot = load(database, dir, schema).await?;
            Ok(Some("reloaded".to_owned()))
        }
        Effect::Apply(versions) => {
            let migrations =
                MigrationSet::from_directory(dir).map_err(|error| error.to_string())?;
            let migrator = Migrator::new(database, &migrations);
            let applied = migrator
                .up_versions(&versions)
                .await
                .map_err(|error| error.to_string())?;
            *snapshot = load(database, dir, schema).await?;
            Ok(Some(format!("applied {}", applied.join(", "))))
        }
        Effect::ConfirmRename(candidate) => {
            let Some(live) = snapshot.diff.as_mut() else {
                return Ok(None);
            };
            let rewritten = live.confirm_rename(&candidate);
            snapshot.drift = live.changes().iter().map(ToString::to_string).collect();
            snapshot.candidates = live.rename_candidates().to_vec();
            Ok(Some(if rewritten {
                "rename confirmed; the plan now renames instead of dropping".to_owned()
            } else {
                "candidate no longer applies".to_owned()
            }))
        }
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, ui: &Ui) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(1)])
        .split(frame.area());
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(vertical[0]);

    let items: Vec<ListItem<'_>> = ui
        .rows
        .iter()
        .map(|row| {
            // Plain words over symbols: states must be readable at a
            // glance, not decoded.
            let mut spans = vec![
                if row.applied {
                    Span::styled(" applied ", Style::default().fg(Color::Green))
                } else {
                    Span::styled(" pending ", Style::default().fg(Color::Yellow))
                },
                Span::raw(row.version.clone()),
            ];
            if row.destructive {
                spans.push(Span::styled(
                    "  destructive",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let mut list_state = ListState::default();
    list_state.select(Some(ui.selected.min(ui.rows.len().saturating_sub(1))));
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" migrations "))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        columns[0],
        &mut list_state,
    );

    let mut detail: Vec<Line<'_>> = Vec::new();
    match &ui.mode {
        Mode::ConfirmApply {
            versions,
            destructive,
        } => {
            detail.push(Line::from(format!(
                "apply {} migration(s)? [y/n]",
                versions.len()
            )));
            if *destructive {
                detail.push(Line::styled(
                    "includes destructive steps",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ));
            }
            for version in versions {
                detail.push(Line::from(format!("  {version}")));
            }
        }
        Mode::ConfirmRename { candidates } => {
            let candidate = &candidates[0];
            detail.push(Line::from("confirm rename? [y/n]"));
            detail.push(Line::from(match candidate {
                RenameCandidate::Table { from, to } => format!("  table {from} -> {to}"),
                RenameCandidate::Column { table, from, to } => {
                    format!("  column {table}.{from} -> {to}")
                }
            }));
            detail.push(Line::from(
                "y rewrites the drop-plus-add into a rename; n keeps it",
            ));
        }
        Mode::Notice(notice) => detail.push(Line::from(notice.clone())),
        Mode::Browse => {
            if let Some(row) = ui.rows.get(ui.selected) {
                detail.push(Line::styled(
                    row.version.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ));
                for step in &row.steps {
                    detail.push(Line::from(format!("  {step}")));
                }
            }
            if !ui.drift.is_empty() {
                detail.push(Line::from(""));
                detail.push(Line::styled(
                    format!("drift ({} candidates: press n)", ui.candidates.len()),
                    Style::default().fg(Color::Yellow),
                ));
                for change in &ui.drift {
                    detail.push(Line::from(format!("  {change}")));
                }
            }
        }
    }
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" detail ")),
        columns[1],
    );

    frame.render_widget(
        Paragraph::new("↑/↓ select   u apply   n renames   r reload   q quit"),
        vertical[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<MigrationRow> {
        vec![
            MigrationRow {
                version: "0001_init".to_owned(),
                applied: true,
                destructive: false,
                steps: vec!["create table users".to_owned()],
            },
            MigrationRow {
                version: "0002_drop".to_owned(),
                applied: false,
                destructive: true,
                steps: vec!["drop column users.email".to_owned()],
            },
        ]
    }

    #[test]
    fn applying_asks_first_and_names_destructive_steps() {
        let mut ui = Ui::new(rows(), Vec::new(), Vec::new());
        assert_eq!(ui.apply_action(Action::StartApply), Effect::None);
        assert_eq!(
            ui.mode,
            Mode::ConfirmApply {
                versions: vec!["0002_drop".to_owned()],
                destructive: true,
            }
        );
        assert_eq!(
            ui.apply_action(Action::Confirm),
            Effect::Apply(vec!["0002_drop".to_owned()]),
            "confirmation applies exactly the plan that was shown"
        );
        assert_eq!(ui.mode, Mode::Browse);
    }

    #[test]
    fn cancelling_a_confirmation_changes_nothing() {
        let mut ui = Ui::new(rows(), Vec::new(), Vec::new());
        ui.apply_action(Action::StartApply);
        assert_eq!(ui.apply_action(Action::Cancel), Effect::None);
        assert_eq!(ui.mode, Mode::Browse);
    }

    #[test]
    fn rename_review_walks_candidates_and_only_yes_rewrites() {
        use jetorm_schema::TableName;
        let candidates = vec![
            RenameCandidate::Table {
                from: TableName::new("users"),
                to: TableName::new("accounts"),
            },
            RenameCandidate::Column {
                table: TableName::new("posts"),
                from: "title".to_owned(),
                to: "headline".to_owned(),
            },
        ];
        let mut ui = Ui::new(rows(), Vec::new(), candidates.clone());
        ui.apply_action(Action::StartRenameReview);

        // Declining the first moves on without an effect.
        assert_eq!(ui.apply_action(Action::Cancel), Effect::None);
        // Accepting the second emits exactly that candidate.
        assert_eq!(
            ui.apply_action(Action::Confirm),
            Effect::ConfirmRename(candidates[1].clone())
        );
        assert_eq!(ui.mode, Mode::Browse, "the review ends after the last one");
    }

    #[test]
    fn selection_stays_in_bounds() {
        let mut ui = Ui::new(rows(), Vec::new(), Vec::new());
        ui.apply_action(Action::Up);
        assert_eq!(ui.selected, 0);
        ui.apply_action(Action::Down);
        ui.apply_action(Action::Down);
        assert_eq!(ui.selected, 1, "selection cannot pass the last row");
    }
}
