//! Migration notes across the machine's repositories (`docs/migrations.md`).
//!
//! The sources are every configured project's main checkout, the config repository `sources.toml` names, and the
//! `[hooks] orchestrator_repos` notes repositories. Each source's notes are read from its remote default branch, never
//! the working tree. What the machine did with each note is kept in the store.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use headwork_adapters::notes::{NOTES_DIR, NoteRepo};
use headwork_core::migrate::{self, Machine, Note, Settled, Standing, StepKind, standings};
use headwork_core::{Actor, Config};
use headwork_store::Store;
use serde::Serialize;

use super::slashed;
use crate::home::Home;

/// The repository key of the config repository `sources.toml` names.
pub const CONFIG_REPO: &str = "claude-config";

/// The exit code `apply` gives when it stopped at a `confirm` step.
pub const EXIT_CONFIRM: u8 = 3;

/// The exit code `apply` gives when it stopped at a `user` step.
pub const EXIT_USER: u8 = 4;

/// A repository the notes are read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The key notes are addressed and recorded by.
    pub repo: String,
    /// The checkout.
    pub dir: PathBuf,
}

/// Every repository this machine reads notes from, without repeats.
///
/// Keys: a project's name; `claude-config` for the `sources.toml` repository; the lower-case folder name for each
/// `orchestrator_repos` entry. A folder already listed is skipped. A key another repository took gets a suffix: `-config`
/// for the config repository, `-notes` for an orchestrator repository, then `-2`, `-3`, so a key never names two
/// repositories.
#[must_use]
pub fn sources(home: &Home, config: &Config) -> Vec<Source> {
    let mut found: Vec<Source> = Vec::new();
    let mut add = |repo: String, suffix: &str, dir: PathBuf| {
        let same = slashed(&dir).to_lowercase();
        if found
            .iter()
            .any(|have| slashed(&have.dir).to_lowercase() == same)
        {
            return;
        }
        let mut key = repo.clone();
        if found.iter().any(|have| have.repo == key) {
            key = format!("{repo}{suffix}");
        }
        let mut count = 1;
        while found.iter().any(|have| have.repo == key) {
            count += 1;
            key = format!("{repo}{suffix}-{count}");
        }
        found.push(Source { repo: key, dir });
    };
    for (name, project) in &config.projects {
        add(name.to_string(), "", home.expand(&project.main));
    }
    if let Ok(Some(layout)) = super::config_sync::Layout::load(home) {
        add(CONFIG_REPO.to_owned(), "-config", layout.repo);
    }
    for dir in &config.hooks.orchestrator_repos {
        let dir = home.expand(dir);
        let repo = dir.file_name().map_or_else(
            || "notes".to_owned(),
            |n| n.to_string_lossy().to_lowercase(),
        );
        add(repo, "-notes", dir);
    }
    found
}

/// What the note filters are compared with on this machine.
#[must_use]
pub fn machine(home: &Home, config: &Config) -> Machine {
    Machine {
        os: std::env::consts::OS.to_owned(),
        name: super::config_sync::Layout::load(home)
            .ok()
            .flatten()
            .map(|layout| layout.machine),
        projects: config.projects.keys().map(ToString::to_string).collect(),
    }
}

/// One note of one repository, with where it stands.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    /// The repository key.
    pub repo: String,
    /// The note's id (its file name without `.toml`).
    pub id: String,
    /// Its title; empty when the file does not parse.
    pub title: String,
    /// Where it stands on this machine; [`Standing::Invalid`] for a file that does not validate.
    pub standing: Standing,
    /// Its path in the repository.
    pub file: String,
    /// Whether the file is in the checkout's `HEAD` already.
    pub in_head: bool,
    /// What is wrong with the file.
    pub problems: Vec<String>,
    #[serde(skip)]
    note: Option<Note>,
    #[serde(skip)]
    dir: PathBuf,
}

impl Entry {
    /// `<repo>/<id>`, the address that is never ambiguous.
    #[must_use]
    pub fn address(&self) -> String {
        format!("{}/{}", self.repo, self.id)
    }

    /// Whether it applies here and is not settled.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.standing == Standing::Pending
    }

    /// The parsed note, when the file is valid.
    #[must_use]
    pub const fn note(&self) -> Option<&Note> {
        self.note.as_ref()
    }
}

/// The notes of every source.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Every note found.
    pub entries: Vec<Entry>,
    /// Repositories that could not be fetched or read.
    pub errors: Vec<SourceError>,
}

/// A repository that could not be fetched or read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceError {
    /// The repository key.
    pub repo: String,
    /// What failed.
    pub message: String,
    /// Whether its notes could not be read at all, so the report holds none of them; a failed fetch alone leaves the
    /// notes of the refs fetched before.
    pub unread: bool,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.repo, self.message)
    }
}

impl Report {
    /// The notes that apply here and wait.
    #[must_use]
    pub fn pending(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.is_pending()).collect()
    }

    /// The note an address names: `<repo>/<id>`, or a bare `<id>` that only one repository has.
    ///
    /// # Errors
    ///
    /// Fails when no note matches or a bare id matches in several repositories.
    pub fn find(&self, address: &str) -> Result<&Entry> {
        let matches: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| entry.id == address || entry.address() == address)
            .collect();
        match matches.as_slice() {
            [one] => Ok(one),
            [] => bail!("no migration note `{address}`; `headwork migrate list` shows them"),
            many => bail!(
                "`{address}` is in several repositories: {}",
                many.iter()
                    .map(|entry| entry.address())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.entries.is_empty() {
            writeln!(f, "no migration notes")?;
        }
        for entry in &self.entries {
            write!(f, "{:<15} {}", entry.standing.as_str(), entry.address())?;
            if !entry.title.is_empty() {
                write!(f, "  {}", entry.title)?;
            }
            writeln!(f)?;
            for problem in &entry.problems {
                writeln!(f, "                  {problem}")?;
            }
        }
        for error in &self.errors {
            writeln!(f, "problem: {error}")?;
        }
        Ok(())
    }
}

/// Reads the notes of every source.
///
/// With `fetch`, each source's default branch is fetched first; a failed fetch is reported and the refs fetched
/// before are read. Without it nothing touches the network.
///
/// # Errors
///
/// Fails when the store cannot be read.
pub fn report(store: &Store, home: &Home, config: &Config, fetch: bool) -> Result<Report> {
    let here = machine(home, config);
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    for source in sources(home, config) {
        let repo = NoteRepo::new(&source.dir);
        if !repo.exists() || !repo.has_origin() {
            continue;
        }
        if fetch && let Err(error) = repo.fetch() {
            errors.push(SourceError {
                repo: source.repo.clone(),
                message: format!("fetch failed: {error}"),
                unread: false,
            });
        }
        match read_source(&source, &repo) {
            Ok(mut parsed) => {
                let notes: Vec<Note> = parsed.iter().filter_map(|p| p.note.clone()).collect();
                let recorded = store.settled_migrations(&source.repo)?;
                let stands = standings(&notes, &recorded, &here);
                for entry in &mut parsed {
                    if let Some(note) = entry.note() {
                        entry.standing = stands
                            .iter()
                            .find(|(id, _)| *id == note.id)
                            .map_or(Standing::Invalid, |(_, standing)| *standing);
                    }
                }
                entries.extend(parsed);
            }
            Err(error) => errors.push(SourceError {
                repo: source.repo.clone(),
                message: format!("{error:#}"),
                unread: true,
            }),
        }
    }
    Ok(Report { entries, errors })
}

fn read_source(source: &Source, repo: &NoteRepo) -> Result<Vec<Entry>> {
    let reference = repo.default_ref()?;
    let files = repo.list(&reference)?;
    let texts: Vec<(String, Result<String, String>)> = files
        .iter()
        .map(|file| {
            let text = repo
                .show(&reference, file)
                .map_err(|error| error.to_string());
            (stem_of(Path::new(file)), text)
        })
        .collect();
    Ok(files
        .into_iter()
        .zip(migrate::parse_files(&texts))
        .map(|(file, parsed)| Entry {
            repo: source.repo.clone(),
            id: parsed.stem,
            title: parsed.title,
            standing: Standing::Invalid,
            in_head: repo.in_head(&file),
            file,
            problems: parsed.problems,
            note: parsed.note,
            dir: source.dir.clone(),
        })
        .collect())
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned())
}

/// One note in full, with its steps as they would run on this machine.
#[must_use]
pub fn describe(entry: &Entry, store: &Store, home: &Home, config: &Config) -> String {
    let mut lines = vec![
        format!("{}  [{}]", entry.address(), entry.standing.as_str()),
        entry.title.clone(),
    ];
    if let Ok(Some(row)) = store.migration_row(&entry.repo, &entry.id) {
        lines.push(format!(
            "settled: {} by {} at {}: {}",
            row.state.as_str(),
            row.actor,
            row.at,
            row.reason
        ));
    }
    lines.extend(
        entry
            .problems
            .iter()
            .map(|problem| format!("problem: {problem}")),
    );
    if let Some(note) = entry.note() {
        let name = machine(home, config).name;
        let fields = [
            ("file", entry.file.clone()),
            ("since", note.since.clone().unwrap_or_default()),
            ("os", note.os.join(", ")),
            ("machines", note.machines.join(", ")),
            ("requires", note.requires.join(", ")),
            ("why", note.why.clone()),
            ("check", note.check.clone().unwrap_or_default()),
        ];
        lines.extend(
            fields
                .into_iter()
                .filter(|(_, value)| !value.is_empty())
                .map(|(label, value)| format!("{label}: {value}")),
        );
        if !entry.in_head && note.pull {
            lines.push(
                "pull: not in this checkout yet; apply runs git pull --ff-only first".to_owned(),
            );
        }
        if note.uses_machine() && name.is_none() {
            lines.push(format!(
                "machine: unknown, {} stays in the steps; pass --machine <name> to apply",
                migrate::MACHINE_PLACEHOLDER
            ));
        }
        let steps = name
            .as_deref()
            .map_or_else(|| note.step.clone(), |name| note.steps_for(name));
        for (index, step) in steps.iter().enumerate() {
            let what = step
                .run
                .clone()
                .or_else(|| step.say.clone())
                .unwrap_or_default();
            lines.push(format!("step {}: {} {what}", index + 1, step.kind));
            lines.extend(
                step.backup
                    .iter()
                    .map(|backup| format!("        backup: {backup}")),
            );
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

/// How an `apply` ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Outcome {
    /// Every step ran, or the check found the note applied; it is recorded.
    Applied {
        /// What the steps printed, or why the check counted.
        detail: String,
    },
    /// The note was settled before; nothing ran.
    AlreadySettled {
        /// How.
        state: Settled,
    },
    /// The note does not apply on this machine or its file is invalid; nothing ran.
    NotApplicable,
    /// A step failed; nothing is recorded.
    Failed {
        /// The step, counted from 1; 0 for the pull before the steps.
        step: usize,
        /// What it printed.
        output: String,
    },
    /// A `confirm` or `user` step needs the user.
    Ask {
        /// The step, counted from 1; 0 for the pull before the steps.
        step: usize,
        /// Its kind.
        kind: StepKind,
        /// What to ask: the command, or what the user is to do.
        text: String,
        /// The backup that runs before a confirm step.
        backup: Option<String>,
        /// How to go on once the user agreed or did it.
        resume: String,
    },
}

impl Outcome {
    /// The exit code of `headwork migrate apply`.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Applied { .. } | Self::AlreadySettled { .. } | Self::NotApplicable => 0,
            Self::Failed { .. } => 1,
            Self::Ask {
                kind: StepKind::User,
                ..
            } => EXIT_USER,
            Self::Ask { .. } => EXIT_CONFIRM,
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Applied { detail } => writeln!(f, "applied\n{detail}"),
            Self::AlreadySettled { state } => writeln!(f, "already {}", state.as_str()),
            Self::NotApplicable => writeln!(f, "not applicable on this machine"),
            Self::Failed { step, output } => writeln!(f, "step {step} failed:\n{output}"),
            Self::Ask {
                step,
                kind,
                text,
                backup,
                resume,
            } => {
                writeln!(f, "step {step} is a {kind} step: ask the user first")?;
                writeln!(f, "  {text}")?;
                if let Some(backup) = backup {
                    writeln!(f, "  backup first: {backup}")?;
                }
                writeln!(f, "  then: {resume}")
            }
        }
    }
}

/// What `apply` is asked to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplyOptions {
    /// The machine's name for `{machine}` in the steps, when `sources.toml` does not give it.
    pub machine: Option<String>,
    /// Start at this step (0 is the pull before the steps) instead of the check.
    pub from: Option<usize>,
    /// The user agreed to the step `from` names, or did it.
    pub confirmed: bool,
}

fn locate<'a>(found: &'a Report, address: &str) -> Result<&'a Entry> {
    let entry = found.find(address).map_err(|error| {
        let failed: Vec<String> = found.errors.iter().map(ToString::to_string).collect();
        if failed.is_empty() {
            error
        } else {
            error.context(format!(
                "repositories with a problem: {}",
                failed.join("; ")
            ))
        }
    })?;
    if let Some(error) = found.errors.iter().find(|e| e.repo == entry.repo) {
        bail!(
            "{error}; apply needs the newest notes, so fix the network or the ssh key and run it again"
        );
    }
    Ok(entry)
}

const fn unsettled_outcome(standing: Standing) -> Option<Outcome> {
    match standing {
        Standing::Applied => Some(Outcome::AlreadySettled {
            state: Settled::Applied,
        }),
        Standing::Skipped => Some(Outcome::AlreadySettled {
            state: Settled::Skipped,
        }),
        Standing::NotApplicable | Standing::Invalid => Some(Outcome::NotApplicable),
        Standing::Pending => None,
    }
}

/// Applies a note: the check, then the steps in order, stopping at the first `confirm` or `user` step.
///
/// # Errors
///
/// Fails when the note cannot be found or read, or the store cannot be written.
pub fn apply(
    store: &mut Store,
    home: &Home,
    config: &Config,
    address: &str,
    options: &ApplyOptions,
    actor: Actor,
) -> Result<Outcome> {
    let found = report(store, home, config, options.from.is_none())?;
    let entry = locate(&found, address)?;
    let Some(note) = entry.note() else {
        bail!(
            "`{}` is not a valid note, nothing was run:
  {}",
            entry.address(),
            entry.problems.join(
                "
  "
            )
        );
    };
    if let Some(done) = unsettled_outcome(entry.standing) {
        return Ok(done);
    }
    let repo = NoteRepo::new(&entry.dir);
    let shell = headwork_adapters::shell::resolve(config.machine.shell.as_deref());
    if options.from.is_none()
        && let Some(check) = &note.check
    {
        let ran = repo.run_line(&shell, check)?;
        if ran.success {
            store.settle_migration(
                &entry.repo,
                &entry.id,
                Settled::Applied,
                &ran.output,
                "its check passed",
                actor,
            )?;
            return Ok(Outcome::Applied {
                detail: "its check passed".to_owned(),
            });
        }
    }
    let name = options
        .machine
        .clone()
        .or_else(|| machine(home, config).name);
    if note.uses_machine() && name.is_none() {
        bail!(
            "`{}` names the machine, and this one has no sources.toml to take it from: pass --machine <name>",
            entry.address()
        );
    }
    let steps = note.steps_for(name.as_deref().unwrap_or_default());
    let mut run = Runner {
        repo: &repo,
        shell: &shell,
        address: entry.address(),
        output: String::new(),
    };
    let start = options.from.unwrap_or(0);
    if start > steps.len() {
        bail!(
            "`{}` has {} step(s); --from {start} is past the last one",
            entry.address(),
            steps.len()
        );
    }
    if start == 0
        && !entry.in_head
        && note.pull
        && let Some(outcome) = run.pull(entry, options)?
    {
        return Ok(outcome);
    }
    for (index, step) in steps.iter().enumerate() {
        let number = index + 1;
        if number < start {
            continue;
        }
        let accepted = options.confirmed && Some(number) == options.from;
        if let Some(outcome) = run.step(number, step, accepted) {
            return Ok(outcome);
        }
    }
    store.settle_migration(
        &entry.repo,
        &entry.id,
        Settled::Applied,
        &run.output,
        "applied",
        actor,
    )?;
    Ok(Outcome::Applied { detail: run.output })
}

struct Runner<'a> {
    repo: &'a NoteRepo,
    shell: &'a Path,
    address: String,
    output: String,
}

impl Runner<'_> {
    fn exec(&mut self, number: usize, line: &str) -> Option<Outcome> {
        let (success, printed) = match self.repo.run_line(self.shell, line) {
            Ok(ran) => (ran.success, ran.output),
            Err(error) => (false, error.to_string()),
        };
        self.output = format!("{}$ {line}\n{printed}\n", self.output);
        (!success).then(|| Outcome::Failed {
            step: number,
            output: self.output.clone(),
        })
    }

    fn resume(&self, number: usize) -> String {
        format!(
            "headwork migrate apply {} --from {number} --confirmed",
            self.address
        )
    }

    fn pull(&mut self, entry: &Entry, options: &ApplyOptions) -> Result<Option<Outcome>> {
        let reference = self.repo.default_ref()?;
        let default = reference.trim_start_matches("origin/");
        let branch = self.repo.branch()?;
        let on_default = branch == default;
        let clean = !self.repo.dirty()?;
        let accepted = options.confirmed && options.from == Some(0);
        if on_default && clean {
            return Ok(self.exec(0, "git pull --ff-only"));
        }
        if !accepted {
            return Ok(Some(Outcome::Ask {
                step: 0,
                kind: StepKind::Confirm,
                text: format!(
                    "git pull --ff-only in {}: the checkout has changes or is not on {default}",
                    slashed(&entry.dir)
                ),
                backup: None,
                resume: self.resume(0),
            }));
        }
        if !on_default {
            if !clean {
                self.output = format!(
                    "{}the checkout is on `{branch}` with changes; commit or stash them and check out `{default}` first\n",
                    self.output
                );
                return Ok(Some(Outcome::Failed {
                    step: 0,
                    output: self.output.clone(),
                }));
            }
            if let Some(failed) = self.exec(0, &format!("git checkout {default}")) {
                return Ok(Some(failed));
            }
        }
        Ok(self.exec(0, "git pull --ff-only"))
    }

    fn step(&mut self, number: usize, step: &migrate::Step, accepted: bool) -> Option<Outcome> {
        let line = step.run.clone().unwrap_or_default();
        match step.kind {
            StepKind::Agent => self.exec(number, &line),
            StepKind::Confirm if accepted => {
                if let Some(backup) = &step.backup
                    && let Some(failed) = self.exec(number, backup)
                {
                    return Some(failed);
                }
                self.exec(number, &line)
            }
            StepKind::Confirm => Some(Outcome::Ask {
                step: number,
                kind: StepKind::Confirm,
                text: line,
                backup: step.backup.clone(),
                resume: self.resume(number),
            }),
            StepKind::User if accepted => None,
            StepKind::User => Some(Outcome::Ask {
                step: number,
                kind: StepKind::User,
                text: step.say.clone().unwrap_or_default(),
                backup: None,
                resume: self.resume(number),
            }),
        }
    }
}

/// Records a note as applied or skipped by hand.
///
/// # Errors
///
/// Fails when the note cannot be found, the reason is empty, or the store cannot be written.
pub fn mark(
    store: &mut Store,
    home: &Home,
    config: &Config,
    address: &str,
    state: Settled,
    reason: &str,
    actor: Actor,
) -> Result<String> {
    if reason.trim().is_empty() {
        bail!("a reason is required");
    }
    let found = report(store, home, config, false)?;
    let entry = found.find(address)?;
    store.settle_migration(&entry.repo, &entry.id, state, "", reason, actor)?;
    Ok(format!("{} marked {}", entry.address(), state.as_str()))
}

/// What `migrate check` found in a folder's `migrations/`.
#[derive(Debug, Clone, Serialize)]
pub struct Checked {
    /// How many note files were read.
    pub files: usize,
    /// What is wrong, as `<file>: <problem>`.
    pub problems: Vec<String>,
}

/// Validates the notes in `<dir>/migrations/` of the working tree, for gates.
///
/// # Errors
///
/// Fails when the folder exists but cannot be read.
pub fn check(dir: &Path) -> Result<Checked> {
    let notes = dir.join(NOTES_DIR);
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&notes) {
        Ok(read) => read
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
            })
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", notes.display())),
    };
    files.sort();
    let texts: Vec<(String, Result<String, String>)> = files
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).map_err(|error| error.to_string());
            (stem_of(path), text)
        })
        .collect();
    let mut problems = Vec::new();
    for (path, parsed) in files.iter().zip(migrate::parse_files(&texts)) {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        problems.extend(parsed.problems.iter().map(|p| format!("{name}: {p}")));
    }
    Ok(Checked {
        files: files.len(),
        problems,
    })
}

impl fmt::Display for Checked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for problem in &self.problems {
            writeln!(f, "{problem}")?;
        }
        if self.problems.is_empty() {
            writeln!(f, "{} migration note(s) are valid", self.files)?;
        }
        Ok(())
    }
}

/// The addresses of the pending notes, with the fetch first when `fetch` is set; `doctor` passes `false` to read the
/// refs fetched before.
///
/// # Errors
///
/// Fails when the store cannot be read.
pub fn pending(store: &Store, home: &Home, config: &Config, fetch: bool) -> Result<Vec<String>> {
    Ok(report(store, home, config, fetch)?
        .pending()
        .iter()
        .map(|entry| entry.address())
        .collect())
}

/// Leaves a notice for the orchestrator about pending notes it was not told of, once each; used after `config sync`.
///
/// Does nothing while an update holds the lock: the update tells them itself.
///
/// # Errors
///
/// Fails when the store, the lock or `update.json` cannot be handled.
pub fn announce(home: &Home, config: &Config) -> Result<()> {
    let store = home.open_store()?;
    let addresses = pending(&store, home, config, true)?;
    if addresses.is_empty() {
        return Ok(());
    }
    let Some(_lock) = super::update::UpdateLock::try_take(home)? else {
        return Ok(());
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(0));
    let mut state = super::update::UpdateState::load(home);
    state.tell_migrations(now, &addresses);
    state.save(home)
}

/// The line the orchestrator is told, for notes it was not told about yet; `None` when there are none.
///
/// `told` holds the addresses told before and gains the new ones.
#[must_use]
pub fn announcement(pending: &[String], told: &mut BTreeSet<String>) -> Option<String> {
    let fresh: Vec<&String> = pending.iter().filter(|a| !told.contains(*a)).collect();
    if fresh.is_empty() {
        return None;
    }
    let text = format!(
        "{} migration note(s) pending: {}; `headwork migrate list` shows them, `headwork migrate apply <note>` runs one",
        fresh.len(),
        fresh
            .iter()
            .map(|a| a.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    told.extend(fresh.into_iter().cloned());
    Some(text)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests fail loudly on a bad fixture")]
mod tests {
    use super::*;

    fn pending(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_note_is_announced_once() {
        let mut told = BTreeSet::new();
        let first = announcement(&pending(&["a/x", "a/y"]), &mut told).unwrap();
        assert!(
            first.starts_with("2 migration note(s) pending: a/x, a/y"),
            "{first}"
        );
        assert_eq!(
            announcement(&pending(&["a/x", "a/y"]), &mut told),
            None,
            "told already"
        );
        let next = announcement(&pending(&["a/x", "b/z"]), &mut told).unwrap();
        assert!(
            next.starts_with("1 migration note(s) pending: b/z"),
            "{next}"
        );
        assert_eq!(announcement(&[], &mut told), None, "nothing pending");
    }

    fn entry(repo: &str, id: &str) -> Entry {
        Entry {
            repo: repo.to_owned(),
            id: id.to_owned(),
            title: String::new(),
            standing: Standing::Pending,
            file: format!("migrations/{id}.toml"),
            in_head: true,
            problems: Vec::new(),
            note: None,
            dir: PathBuf::new(),
        }
    }

    #[test]
    fn addresses_resolve_by_repo_or_by_a_unique_id() {
        let report = Report {
            entries: vec![entry("a", "one"), entry("b", "one"), entry("b", "two")],
            errors: Vec::new(),
        };
        assert_eq!(report.find("b/one").unwrap().repo, "b", "full address");
        assert_eq!(report.find("two").unwrap().repo, "b", "unique id");
        assert!(
            report
                .find("one")
                .unwrap_err()
                .to_string()
                .contains("a/one, b/one"),
            "ambiguous"
        );
        assert!(report.find("none").is_err(), "unknown");
    }

    #[test]
    fn exit_codes_tell_the_outcomes_apart() {
        let ask = |kind| Outcome::Ask {
            step: 1,
            kind,
            text: String::new(),
            backup: None,
            resume: String::new(),
        };
        let codes = [
            Outcome::Applied {
                detail: String::new(),
            }
            .exit_code(),
            Outcome::Failed {
                step: 1,
                output: String::new(),
            }
            .exit_code(),
            ask(StepKind::Confirm).exit_code(),
            ask(StepKind::User).exit_code(),
        ];
        assert_eq!(codes, [0, 1, EXIT_CONFIRM, EXIT_USER], "exit codes");
    }

    #[test]
    fn check_reads_the_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join(NOTES_DIR);
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(
            notes.join("good.toml"),
            "id = \"good\"\ntitle = \"t\"\n[[step]]\nkind = \"user\"\nsay = \"do it\"\n",
        )
        .unwrap();
        std::fs::write(
            notes.join("bad.toml"),
            "id = \"other\"\ntitle = \"t\"\n[[step]]\nkind = \"agent\"\nrun = \"sudo ls\"\n",
        )
        .unwrap();
        let checked = check(dir.path()).unwrap();
        assert_eq!(checked.files, 2, "two files");
        assert_eq!(checked.problems.len(), 2, "{:?}", checked.problems);
        assert!(
            check(&dir.path().join("none")).unwrap().problems.is_empty(),
            "no folder is no problem"
        );
    }
}
