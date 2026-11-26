//! The machine-wide gate folders (`docs/decisions/2026-11-26-gates-in-headwork.md`): slots that heavy commands hold
//! while they run, shared by every headwork build and the old `heavy.sh` scripts.
//!
//! A class has one folder under the root, `<dir>/cpu` and `<dir>/gpu`. A slot is taken by creating `slot-N` in it
//! (an atomic `create_dir`) and writing its `owner` file; it is given back by removing the folder. A waiter keeps a
//! `wait-<pid>` file beside the slots while it waits, so waiting is first come first served. The legacy root
//! `<dir>/slot-N` of the old `heavy.sh` counts as `cpu`. The decisions are pure, in [`headwork_core::gate`].

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use headwork_core::config::GatesConfig;
use headwork_core::gate::{
    GateClass, GateEnv, HolderLine, OWNER_FILE, Owner, REAP_LOCK, REAP_LOCK_STALE_S,
    UNOWNED_STALE_S, first_free_slot, free_slots, may_take, must_give_back, pid_taken_over,
    slot_number, wait_message, wait_pid,
};

/// Overrides the slot root (tests).
pub const DIR_VAR: &str = "HEADWORK_GATE_DIR";
/// The classes the process tree already holds, comma-separated.
pub const HELD_VAR: &str = "HEADWORK_GATE_HELD";
/// The old `heavy.sh`'s mark of a held slot; `1` counts as `cpu`.
pub const HEAVY_HELD_VAR: &str = "HEADWORK_HEAVY_HELD";
/// `1` runs gated commands directly.
pub const OFF_VAR: &str = "HEADWORK_GATE_OFF";
/// Set in CI, where gated commands run directly.
pub const CI_VAR: &str = "CI";

/// How many more times a slot folder's removal is tried after it fails.
const REMOVE_TRIES: u32 = 5;

/// The suffix of a file written before it is renamed into place.
const TEMP_SUFFIX: &str = ".tmp";

/// Why a gate operation failed.
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    /// A file or folder could not be read, made or removed.
    #[error("{action} {path}: {source}")]
    Io {
        /// What was being done.
        action: &'static str,
        /// The file or folder.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: io::Error,
    },
}

fn io_error(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> GateError {
    let path = path.to_path_buf();
    move |source| GateError::Io {
        action,
        path,
        source,
    }
}

/// What the environment of this process says about gating.
#[must_use]
pub fn env_from_process() -> GateEnv {
    GateEnv {
        ci: std::env::var_os(CI_VAR).is_some(),
        off: std::env::var(OFF_VAR).is_ok_and(|value| value == "1"),
        held: std::env::var(HELD_VAR).unwrap_or_default(),
        heavy_held: std::env::var(HEAVY_HELD_VAR).is_ok_and(|value| value == "1"),
    }
}

/// The slot root: `$HEADWORK_GATE_DIR`, else `configured` (`[gates] dir`, already expanded), else [`default_dir`].
#[must_use]
pub fn resolve_dir(configured: Option<PathBuf>) -> Option<PathBuf> {
    std::env::var_os(DIR_VAR)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or(configured)
        .or_else(default_dir)
}

/// `%LOCALAPPDATA%/heavy-slots`, else `~/.heavy-slots`, the root the old `heavy.sh` uses.
#[must_use]
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join("heavy-slots"))
        .or_else(|| std::env::home_dir().map(|home| home.join(".heavy-slots")))
}

/// Seconds since the Unix epoch.
#[must_use]
pub fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Turns Ctrl+C and Ctrl+Break into a flag, so a gate outlives the interrupt its command handles and gives its slot
/// back; `None` when the handler cannot be installed.
#[must_use]
pub fn watch_interrupts() -> Option<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    let set = Arc::clone(&flag);
    ctrlc::set_handler(move || set.store(true, Ordering::SeqCst))
        .ok()
        .map(|()| flag)
}

/// What runs under an OS-native process id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeProcess {
    /// Nothing.
    Gone,
    /// A process, started at `started` (seconds since the Unix epoch) when that could be read.
    Running {
        /// Its start time, when known.
        started: Option<u64>,
    },
}

/// Asks whether a holder's process runs.
pub trait Liveness: fmt::Debug + Send + Sync {
    /// What runs under this OS-native id.
    fn native(&self, pid: u32) -> NativeProcess;
    /// The Git Bash (MSYS) pids that run, or `None` when they cannot be read; asked on Windows only.
    fn git_bash(&self) -> Option<HashSet<u32>>;
}

/// The system's processes.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLiveness;

impl Liveness for SystemLiveness {
    /// On Windows by the system's process list (`tasklist`, which [`crate::proc::is_alive`] runs, takes a third of a
    /// second, too slow for a poll every second); elsewhere by [`crate::proc::is_alive`], which also knows a zombie
    /// is gone. The start time comes from [`crate::proc::identify`]. A pid that cannot be checked counts as running.
    fn native(&self, pid: u32) -> NativeProcess {
        let running = |identity: Option<crate::proc::ProcessIdentity>| NativeProcess::Running {
            started: identity.map(|identity| identity.started),
        };
        if cfg!(windows) {
            return crate::proc::identify(pid)
                .map_or(NativeProcess::Gone, |identity| running(Some(identity)));
        }
        match crate::proc::is_alive(pid) {
            Ok(false) => NativeProcess::Gone,
            Ok(true) | Err(_) => running(crate::proc::identify(pid)),
        }
    }

    fn git_bash(&self) -> Option<HashSet<u32>> {
        crate::proc::git_bash_pids()
    }
}

/// One pass of liveness questions: each pid is asked once, Git Bash's table read at most once.
struct Probe<'a> {
    liveness: &'a dyn Liveness,
    native: HashMap<u32, NativeProcess>,
    git_bash: GitBashTable,
}

/// Git Bash's pids within one pass.
enum GitBashTable {
    /// Not asked yet.
    Unread,
    /// Asked; they could not be read.
    Unknown,
    /// Asked and read.
    Read(HashSet<u32>),
}

impl<'a> Probe<'a> {
    fn new(liveness: &'a dyn Liveness) -> Self {
        Self {
            liveness,
            native: HashMap::new(),
            git_bash: GitBashTable::Unread,
        }
    }

    /// Whether the owner's process runs. A process that started more than
    /// [`headwork_core::gate::PID_REUSE_SLACK_S`] seconds after the owner's `epoch` took its pid over, so the owner is
    /// gone ([`pid_taken_over`]). An owner without `native=1` on Windows was written by a Git Bash script, so its pid
    /// is looked up in Git Bash's table; when that table cannot be read it counts as running.
    fn alive(&mut self, owner: &Owner) -> bool {
        if owner.native || !cfg!(windows) {
            let liveness = self.liveness;
            let now = *self
                .native
                .entry(owner.pid)
                .or_insert_with(|| liveness.native(owner.pid));
            return match now {
                NativeProcess::Gone => false,
                NativeProcess::Running { started } => !pid_taken_over(owner.epoch, started),
            };
        }
        if matches!(self.git_bash, GitBashTable::Unread) {
            self.git_bash = self
                .liveness
                .git_bash()
                .map_or(GitBashTable::Unknown, GitBashTable::Read);
        }
        match &self.git_bash {
            GitBashTable::Read(pids) => pids.contains(&owner.pid),
            GitBashTable::Unread | GitBashTable::Unknown => true,
        }
    }
}

/// A slot folder as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderInfo {
    /// The slot number.
    pub slot: u32,
    /// The slot folder.
    pub path: PathBuf,
    /// Whether it is a legacy root slot of the old `heavy.sh`.
    pub legacy: bool,
    /// Its owner, when the file exists and parses.
    pub owner: Option<Owner>,
    /// Seconds it has been held: since the owner's `epoch`, else since the folder or its owner file last changed.
    pub age_s: u64,
    /// Whether its holder is gone (or its owner file was never written for over [`UNOWNED_STALE_S`] seconds).
    pub stale: bool,
}

impl HolderInfo {
    /// The holder as a wait message shows it.
    #[must_use]
    pub fn line(&self) -> HolderLine {
        self.owner.as_ref().map_or_else(
            || HolderLine {
                who: String::new(),
                command: String::new(),
                held_s: self.age_s,
            },
            |owner| HolderLine::of(owner, self.age_s),
        )
    }
}

/// A waiter's file as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaiterInfo {
    /// The pid in the file name.
    pub pid: u32,
    /// The file.
    pub path: PathBuf,
    /// What it holds, when it parses.
    pub owner: Option<Owner>,
    /// Seconds it has waited: since its `epoch`, else since the file last changed.
    pub age_s: u64,
    /// Whether the waiter is gone.
    pub stale: bool,
}

/// One class's slots and waiters, as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassView {
    /// The class.
    pub class: GateClass,
    /// Its slot count.
    pub count: NonZeroU32,
    /// The class's slot folders, by number.
    pub holders: Vec<HolderInfo>,
    /// The legacy root slots (`cpu` only), by number.
    pub legacy: Vec<HolderInfo>,
    /// The waiters, first come first.
    pub waiters: Vec<WaiterInfo>,
}

impl ClassView {
    /// The holders whose process runs, the class's first, then legacy ones.
    pub fn live_holders(&self) -> impl Iterator<Item = &HolderInfo> {
        self.holders
            .iter()
            .chain(&self.legacy)
            .filter(|holder| !holder.stale)
    }

    /// How many slots live holders take.
    #[must_use]
    pub fn used(&self) -> u32 {
        u32::try_from(self.live_holders().count()).unwrap_or(u32::MAX)
    }

    /// The waiters whose process runs.
    pub fn live_waiters(&self) -> impl Iterator<Item = &WaiterInfo> {
        self.waiters.iter().filter(|waiter| !waiter.stale)
    }

    /// The live waiters that came before `owner`, other than `owner`'s own file.
    #[must_use]
    pub fn waiters_before(&self, owner: &Owner) -> u32 {
        let count = self
            .live_waiters()
            .filter(|waiter| waiter.pid != owner.pid)
            .filter(|waiter| {
                waiter
                    .owner
                    .as_ref()
                    .is_some_and(|theirs| theirs.is_before(owner))
            })
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// `waiting for cpu (3/3: me-sim-x cargo test 4m, ...)` with `ahead` waiters before this one.
    #[must_use]
    pub fn wait_message(&self, ahead: u32) -> String {
        let lines: Vec<HolderLine> = self.live_holders().map(HolderInfo::line).collect();
        wait_message(self.class, self.count, &lines, ahead)
    }
}

/// What [`Slots::reap`] took away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaped {
    /// The slot folder or waiter file.
    pub path: PathBuf,
    /// Its owner, when it had one that parsed.
    pub owner: Option<Owner>,
    /// Whether it was a waiter's file rather than a slot.
    pub waiter: bool,
}

/// What one [`Slots::reap`] did: what it took away, and the first removal that failed.
#[derive(Debug, Default)]
pub struct Reaping {
    /// The slots and files taken away.
    pub reaped: Vec<Reaped>,
    /// The first removal that failed; the others were still tried.
    pub error: Option<GateError>,
}

impl Reaping {
    fn fail(&mut self, error: GateError) {
        self.error.get_or_insert(error);
    }
}

/// The slot root and how holders are judged alive.
#[derive(Debug, Clone)]
pub struct Slots {
    root: PathBuf,
    liveness: Arc<dyn Liveness>,
}

impl Slots {
    /// The slots under `root`, judged by the system's processes.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self::with_liveness(root, Arc::new(SystemLiveness))
    }

    /// The slots under `root`, judged by `liveness`.
    #[must_use]
    pub fn with_liveness(root: PathBuf, liveness: Arc<dyn Liveness>) -> Self {
        Self { root, liveness }
    }

    /// The slot root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder of a class's slots and waiters.
    #[must_use]
    pub fn class_dir(&self, class: GateClass) -> PathBuf {
        self.root.join(class.as_str())
    }

    /// Reads a class's slots, its legacy slots for `cpu`, and its waiters; missing folders read as empty. Writes
    /// nothing.
    #[must_use]
    pub fn read(&self, class: GateClass, count: NonZeroU32) -> ClassView {
        let mut probe = Probe::new(self.liveness.as_ref());
        self.read_with(class, count, &mut probe)
    }

    /// Reads both classes with the counts `[gates]` gives.
    #[must_use]
    pub fn read_all(&self, gates: &GatesConfig) -> Vec<ClassView> {
        let mut probe = Probe::new(self.liveness.as_ref());
        GateClass::ALL
            .iter()
            .map(|&class| self.read_with(class, gates.count(class), &mut probe))
            .collect()
    }

    fn read_with(&self, class: GateClass, count: NonZeroU32, probe: &mut Probe<'_>) -> ClassView {
        let now = epoch_now();
        let dir = self.class_dir(class);
        let holders = read_slots(&dir, false, now, probe);
        let legacy = if class == GateClass::Cpu {
            read_slots(&self.root, true, now, probe)
        } else {
            Vec::new()
        };
        let mut waiters = read_waiters(&dir, now, probe);
        waiters.sort_by(|a, b| match (&a.owner, &b.owner) {
            (Some(x), Some(y)) if x.is_before(y) => std::cmp::Ordering::Less,
            (Some(x), Some(y)) if y.is_before(x) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => a.pid.cmp(&b.pid),
        });
        ClassView {
            class,
            count,
            holders,
            legacy,
            waiters,
        }
    }

    /// Takes away a class's stale slots (and for `cpu` the stale legacy root slots), the files of waiters that are
    /// gone, and half-written files older than [`UNOWNED_STALE_S`] seconds.
    ///
    /// A slot is removed only while its folder's `reap.lock` is held and after a second look, with fresh liveness,
    /// still finds it stale; a lock older than [`REAP_LOCK_STALE_S`] seconds is removed first. A lock held by another
    /// reaper skips the slot. A removal that fails does not stop the others: the result lists what was removed and
    /// keeps the first failure.
    #[must_use]
    pub fn reap(&self, class: GateClass) -> Reaping {
        let mut reaping = Reaping::default();
        let now = epoch_now();
        let dir = self.class_dir(class);
        let mut probe = Probe::new(self.liveness.as_ref());
        let mut places = vec![(dir.clone(), false)];
        if class == GateClass::Cpu {
            places.push((self.root.clone(), true));
        }
        for (place, legacy) in places {
            let stale: Vec<HolderInfo> = read_slots(&place, legacy, now, &mut probe)
                .into_iter()
                .filter(|holder| holder.stale)
                .collect();
            if !stale.is_empty() {
                self.reap_slots(&place, legacy, &stale, &mut reaping);
            }
        }
        for waiter in read_waiters(&dir, now, &mut probe) {
            if waiter.stale {
                match remove_file(&waiter.path) {
                    Ok(()) => reaping.reaped.push(Reaped {
                        path: waiter.path,
                        owner: waiter.owner,
                        waiter: true,
                    }),
                    Err(error) => reaping.fail(error),
                }
            }
        }
        remove_old_temps(&dir, now, &mut reaping);
        reaping
    }

    fn reap_slots(&self, place: &Path, legacy: bool, stale: &[HolderInfo], reaping: &mut Reaping) {
        let lock = place.join(REAP_LOCK);
        if age_of(&lock, epoch_now()).is_some_and(|age| age > REAP_LOCK_STALE_S) {
            std::fs::remove_dir(&lock).ok();
        }
        if std::fs::create_dir(&lock).is_err() {
            return;
        }
        self.reap_locked(legacy, stale, reaping);
        std::fs::remove_dir(&lock).ok();
    }

    fn reap_locked(&self, legacy: bool, stale: &[HolderInfo], reaping: &mut Reaping) {
        let mut probe = Probe::new(self.liveness.as_ref());
        let now = epoch_now();
        for holder in stale {
            let again = read_slot(&holder.path, holder.slot, legacy, now, &mut probe);
            if again.is_some_and(|again| again.stale) {
                match remove_tree(&holder.path) {
                    Ok(()) => reaping.reaped.push(Reaped {
                        path: holder.path.clone(),
                        owner: holder.owner.clone(),
                        waiter: false,
                    }),
                    Err(error) => reaping.fail(error),
                }
            }
        }
    }

    /// Joins a class's queue: writes this process's `wait-<pid>` file (the owner's `pid`), creating the folders.
    ///
    /// # Errors
    ///
    /// Returns [`GateError::Io`] when the folder or the file cannot be written.
    pub fn enqueue(
        &self,
        class: GateClass,
        count: NonZeroU32,
        owner: Owner,
    ) -> Result<Ticket, GateError> {
        let dir = self.class_dir(class);
        std::fs::create_dir_all(&dir).map_err(io_error("creating", &dir))?;
        let wait_file = dir.join(format!("{}{}", headwork_core::gate::WAIT_PREFIX, owner.pid));
        let owner = Owner {
            class: Some(class),
            ..owner
        };
        write_atomically(&wait_file, &owner.render())?;
        Ok(Ticket {
            slots: self.clone(),
            class,
            count,
            owner,
            wait_file: Some(wait_file),
        })
    }
}

impl Slots {
    /// Takes a free slot of a class for the process `owner.pid` names, without waiting and without joining the queue.
    ///
    /// Stale slots are reaped first. The slot stays taken after this returns; it ends with its holder, as any slot.
    /// Returns the slot number, or `None` when no slot is free (or the live waiters ahead need the free ones).
    ///
    /// # Errors
    ///
    /// Returns [`GateError::Io`] when a slot cannot be made or written.
    pub fn claim_for(
        &self,
        class: GateClass,
        count: NonZeroU32,
        owner: Owner,
        started: &str,
        epoch: u64,
    ) -> Result<Option<u32>, GateError> {
        let dir = self.class_dir(class);
        std::fs::create_dir_all(&dir).map_err(io_error("creating", &dir))?;
        drop(self.reap(class));
        let view = self.read(class, count);
        if free_slots(count, view.used()) == 0 {
            return Ok(None);
        }
        let ticket = Ticket {
            slots: self.clone(),
            class,
            count,
            owner: Owner {
                class: Some(class),
                native: true,
                ..owner
            },
            wait_file: None,
        };
        let Some(held) = ticket.claim(&view, started, epoch)? else {
            return Ok(None);
        };
        let slot = held.slot();
        held.keep();
        Ok(Some(slot))
    }
}

/// A place in a class's queue; dropping it leaves the queue.
#[derive(Debug)]
pub struct Ticket {
    slots: Slots,
    class: GateClass,
    count: NonZeroU32,
    owner: Owner,
    wait_file: Option<PathBuf>,
}

/// What one attempt to take a slot came to.
#[derive(Debug)]
pub enum Attempt {
    /// A slot is held.
    Held(Held),
    /// No slot this time.
    Wait {
        /// The class as the attempt read it.
        view: ClassView,
        /// Live waiters that came before this one.
        ahead: u32,
    },
}

impl Ticket {
    /// The owner this ticket waits as.
    #[must_use]
    pub const fn owner(&self) -> &Owner {
        &self.owner
    }

    /// Tries once to take a slot: takes stale slots and dead waiters' files away, then takes the first free slot when
    /// fewer live waiters came before this one than slots are free. A `cpu` holder that then finds the older holders
    /// and the live legacy slots fill the count gives the slot back and keeps waiting. The slot's owner file records
    /// `started` (local time) and `epoch` as the moment it was taken. On success the wait file is removed.
    ///
    /// # Errors
    ///
    /// Returns [`GateError::Io`] when a slot cannot be made or written.
    pub fn try_take(&mut self, started: &str, epoch: u64) -> Result<Attempt, GateError> {
        drop(self.slots.reap(self.class));
        let view = self.slots.read(self.class, self.count);
        let ahead = view.waiters_before(&self.owner);
        if !may_take(free_slots(self.count, view.used()), ahead) {
            return Ok(Attempt::Wait { view, ahead });
        }
        let Some(held) = self.claim(&view, started, epoch)? else {
            return Ok(Attempt::Wait { view, ahead });
        };
        let Some(held) = self.settle(held)? else {
            let view = self.slots.read(self.class, self.count);
            let ahead = view.waiters_before(&self.owner);
            return Ok(Attempt::Wait { view, ahead });
        };
        self.leave();
        Ok(Attempt::Held(held))
    }

    /// Makes the first free slot folder and writes its owner; `None` when another process made each free one first.
    fn claim(
        &self,
        view: &ClassView,
        started: &str,
        epoch: u64,
    ) -> Result<Option<Held>, GateError> {
        let dir = self.slots.class_dir(self.class);
        let mut taken: Vec<u32> = view.holders.iter().map(|holder| holder.slot).collect();
        while let Some(slot) = first_free_slot(self.count, &taken) {
            let path = dir.join(format!("{}{slot}", headwork_core::gate::SLOT_PREFIX));
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    let held = Held {
                        path,
                        slot,
                        class: self.class,
                        released: false,
                    };
                    let owner = Owner {
                        started: started.to_owned(),
                        epoch: Some(epoch),
                        ..self.owner.clone()
                    };
                    write_atomically(&held.path.join(OWNER_FILE), &owner.render())?;
                    return Ok(Some(held));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => taken.push(slot),
                Err(error) => return Err(io_error("creating", &path)(error)),
            }
        }
        Ok(None)
    }

    /// Keeps a just-taken `cpu` slot only while the older holders and the live legacy slots leave room for it.
    fn settle(&self, held: Held) -> Result<Option<Held>, GateError> {
        if self.class != GateClass::Cpu {
            return Ok(Some(held));
        }
        let view = self.slots.read(self.class, self.count);
        let legacy = view.legacy.iter().filter(|holder| !holder.stale).count();
        if legacy == 0 {
            return Ok(Some(held));
        }
        let mine = view
            .holders
            .iter()
            .find(|holder| holder.slot == held.slot)
            .and_then(|holder| holder.owner.clone());
        let older = view
            .holders
            .iter()
            .filter(|holder| !holder.stale && holder.slot != held.slot)
            .filter(|holder| match (&holder.owner, &mine) {
                (Some(theirs), Some(mine)) => {
                    theirs.is_before(mine) || (!mine.is_before(theirs) && holder.slot < held.slot)
                }
                _ => true,
            })
            .count();
        let older = u32::try_from(older).unwrap_or(u32::MAX);
        let legacy = u32::try_from(legacy).unwrap_or(u32::MAX);
        if must_give_back(self.count, older, legacy) {
            held.release()?;
            return Ok(None);
        }
        Ok(Some(held))
    }

    fn leave(&mut self) {
        if let Some(file) = self.wait_file.take() {
            std::fs::remove_file(file).ok();
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.leave();
    }
}

/// A held slot; dropping it gives the slot back.
#[derive(Debug)]
pub struct Held {
    path: PathBuf,
    slot: u32,
    class: GateClass,
    released: bool,
}

impl Held {
    /// The slot number.
    #[must_use]
    pub const fn slot(&self) -> u32 {
        self.slot
    }

    /// The slot folder.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The class.
    #[must_use]
    pub const fn class(&self) -> GateClass {
        self.class
    }

    /// Leaves the slot taken: dropping no longer removes it, so it lasts until its holder's process is gone.
    pub fn keep(mut self) {
        self.released = true;
    }

    /// Gives the slot back by removing its folder.
    ///
    /// # Errors
    ///
    /// Returns [`GateError::Io`] when the folder cannot be removed.
    pub fn release(mut self) -> Result<(), GateError> {
        self.released = true;
        remove_tree(&self.path)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if !self.released {
            std::fs::remove_dir_all(&self.path).ok();
        }
    }
}

fn read_slots(dir: &Path, legacy: bool, now: u64, probe: &mut Probe<'_>) -> Vec<HolderInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut slots: Vec<HolderInfo> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let slot = slot_number(&entry.file_name().to_string_lossy())?;
            read_slot(&entry.path(), slot, legacy, now, probe)
        })
        .collect();
    slots.sort_by_key(|holder| holder.slot);
    slots
}

fn read_slot(
    path: &Path,
    slot: u32,
    legacy: bool,
    now: u64,
    probe: &mut Probe<'_>,
) -> Option<HolderInfo> {
    if !path.is_dir() {
        return None;
    }
    let owner_path = path.join(OWNER_FILE);
    let owner = std::fs::read_to_string(&owner_path)
        .ok()
        .and_then(|text| Owner::parse(&text).ok());
    let changed = age_of(&owner_path, now)
        .or_else(|| age_of(path, now))
        .unwrap_or(0);
    let (age_s, stale) = owner
        .as_ref()
        .map_or((changed, changed > UNOWNED_STALE_S), |owner| {
            (held_since(owner, changed, now), !probe.alive(owner))
        });
    Some(HolderInfo {
        slot,
        path: path.to_path_buf(),
        legacy,
        owner,
        age_s,
        stale,
    })
}

fn read_waiters(dir: &Path, now: u64, probe: &mut Probe<'_>) -> Vec<WaiterInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = wait_pid(&entry.file_name().to_string_lossy())?;
            let path = entry.path();
            let changed = age_of(&path, now).unwrap_or(0);
            let owner = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| Owner::parse(&text).ok());
            let probed = owner.clone().unwrap_or_else(|| Owner {
                pid,
                native: true,
                ..Owner::default()
            });
            let age_s = owner
                .as_ref()
                .map_or(changed, |owner| held_since(owner, changed, now));
            let stale = (owner.is_some() || changed > UNOWNED_STALE_S) && !probe.alive(&probed);
            Some(WaiterInfo {
                pid,
                path,
                owner,
                age_s,
                stale,
            })
        })
        .collect()
}

/// Seconds an owner has held or waited: since its `epoch`, else `changed`.
fn held_since(owner: &Owner, changed: u64, now: u64) -> u64 {
    owner
        .epoch
        .map_or(changed, |epoch| now.saturating_sub(epoch))
}

/// Seconds since a file or folder last changed; `None` when it is missing.
fn age_of(path: &Path, now: u64) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let at = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(at))
}

/// Writes `text` to a temporary file beside `path` and renames it into place, so a reader never sees half a file.
fn write_atomically(path: &Path, text: &str) -> Result<(), GateError> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(TEMP_SUFFIX);
    let temp = path.with_file_name(name);
    std::fs::write(&temp, text).map_err(io_error("writing", &temp))?;
    std::fs::rename(&temp, path).map_err(|error| {
        std::fs::remove_file(&temp).ok();
        io_error("renaming into", path)(error)
    })
}

fn remove_old_temps(dir: &Path, now: u64, reaping: &mut Reaping) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let temp = entry.file_name().to_string_lossy().ends_with(TEMP_SUFFIX);
        if temp
            && path.is_file()
            && age_of(&path, now).is_some_and(|age| age > UNOWNED_STALE_S)
            && let Err(error) = remove_file(&path)
        {
            reaping.fail(error);
        }
    }
}

fn remove_file(path: &Path) -> Result<(), GateError> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(io_error("removing", path)(error))
        }
        Ok(()) | Err(_) => Ok(()),
    }
}

/// Removes a slot folder, trying a few times: on Windows a reader that has its owner file open for an instant makes
/// the removal fail.
fn remove_tree(path: &Path) -> Result<(), GateError> {
    let mut tries = 0;
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) if tries < REMOVE_TRIES => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => return Err(io_error("removing", path)(error)),
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests fail loudly on an unexpected error"
)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    use super::*;

    #[derive(Debug, Default)]
    struct Fake {
        native: Mutex<HashSet<u32>>,
        started: Mutex<HashMap<u32, u64>>,
        git_bash: Mutex<HashSet<u32>>,
    }

    impl Fake {
        fn live(pids: &[u32]) -> Arc<Self> {
            Arc::new(Self {
                native: Mutex::new(pids.iter().copied().collect()),
                started: Mutex::new(HashMap::new()),
                git_bash: Mutex::new(HashSet::new()),
            })
        }

        fn started_at(&self, pid: u32, started: u64) {
            self.started.lock().unwrap().insert(pid, started);
        }

        fn end(&self, pid: u32) {
            self.native.lock().unwrap().remove(&pid);
        }
    }

    impl Liveness for Fake {
        fn native(&self, pid: u32) -> NativeProcess {
            if !self.native.lock().unwrap().contains(&pid) {
                return NativeProcess::Gone;
            }
            NativeProcess::Running {
                started: self.started.lock().unwrap().get(&pid).copied(),
            }
        }

        fn git_bash(&self) -> Option<HashSet<u32>> {
            Some(self.git_bash.lock().unwrap().clone())
        }
    }

    fn count(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    fn owner(pid: u32, epoch: u64) -> Owner {
        Owner {
            pid,
            native: true,
            task: format!("task-{pid}"),
            command: "cargo test".to_owned(),
            epoch: Some(epoch),
            ..Owner::default()
        }
    }

    fn take(ticket: &mut Ticket) -> Option<Held> {
        match ticket.try_take("2026-11-26 10:00:00", epoch_now()).unwrap() {
            Attempt::Held(held) => Some(held),
            Attempt::Wait { .. } => None,
        }
    }

    fn liveness(fake: &Arc<Fake>) -> Arc<dyn Liveness> {
        let shared: Arc<dyn Liveness> = fake.clone();
        shared
    }

    fn slots(fake: &Arc<Fake>) -> (tempfile::TempDir, Slots) {
        let dir = tempfile::tempdir().unwrap();
        let slots = Slots::with_liveness(dir.path().to_path_buf(), liveness(fake));
        (dir, slots)
    }

    fn write_slot(dir: &Path, slot: u32, text: &str) -> PathBuf {
        let path = dir.join(format!("slot-{slot}"));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join(OWNER_FILE), text).unwrap();
        path
    }

    #[test]
    fn a_slot_is_taken_written_and_given_back() {
        let fake = Fake::live(&[1, 2, 3]);
        let (_dir, slots) = slots(&fake);
        let mut first = slots
            .enqueue(GateClass::Cpu, count(2), owner(1, 10))
            .unwrap();
        let held = take(&mut first).unwrap();
        assert_eq!(held.slot(), 1, "the first slot");
        let written =
            Owner::parse(&std::fs::read_to_string(held.path().join(OWNER_FILE)).unwrap()).unwrap();
        assert_eq!(written.class, Some(GateClass::Cpu), "class recorded");
        assert_eq!(written.started, "2026-11-26 10:00:00", "started recorded");
        assert!(
            !slots.class_dir(GateClass::Cpu).join("wait-1").exists(),
            "the wait file is gone once held"
        );
        let mut second = slots
            .enqueue(GateClass::Cpu, count(2), owner(2, 11))
            .unwrap();
        let other = take(&mut second).unwrap();
        assert_eq!(other.slot(), 2, "the second slot");
        let mut third = slots
            .enqueue(GateClass::Cpu, count(2), owner(3, 12))
            .unwrap();
        let Attempt::Wait { view, ahead } = third.try_take("t", epoch_now()).unwrap() else {
            panic!("a third holder must wait");
        };
        assert_eq!(ahead, 0, "no one ahead");
        assert!(
            view.wait_message(ahead)
                .starts_with("waiting for cpu (2/2: task-1 cargo test"),
            "{}",
            view.wait_message(0)
        );
        held.release().unwrap();
        let again = take(&mut third).unwrap();
        assert_eq!(again.slot(), 1, "the freed slot");
        let path = again.path().to_path_buf();
        drop(again);
        assert!(!path.exists(), "dropping a held slot gives it back");
        drop(third);
        assert!(
            !slots.class_dir(GateClass::Cpu).join("wait-3").exists(),
            "no wait file left"
        );
    }

    #[test]
    fn waiters_are_served_first_come_first_served() {
        let fake = Fake::live(&[1, 2, 3]);
        let (_dir, slots) = slots(&fake);
        let mut holder = slots
            .enqueue(GateClass::Gpu, count(1), owner(1, 10))
            .unwrap();
        let held = take(&mut holder).unwrap();
        let mut early = slots
            .enqueue(GateClass::Gpu, count(1), owner(2, 20))
            .unwrap();
        let mut late = slots
            .enqueue(GateClass::Gpu, count(1), owner(3, 30))
            .unwrap();
        assert!(take(&mut early).is_none(), "the slot is held");
        held.release().unwrap();
        let Attempt::Wait { ahead, .. } = late.try_take("t", epoch_now()).unwrap() else {
            panic!("the later waiter must let the earlier one go first");
        };
        assert_eq!(ahead, 1, "one ahead");
        assert!(
            take(&mut early).is_some(),
            "the earlier waiter takes the slot"
        );
    }

    #[test]
    fn a_dead_waiter_is_skipped_and_its_file_removed() {
        let fake = Fake::live(&[2, 3]);
        let (_dir, slots) = slots(&fake);
        let dead = slots.class_dir(GateClass::Cpu);
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::write(dead.join("wait-9"), owner(9, 1).render()).unwrap();
        let mut ticket = slots
            .enqueue(GateClass::Cpu, count(1), owner(2, 20))
            .unwrap();
        assert!(take(&mut ticket).is_some(), "a dead waiter holds no one up");
        assert!(!dead.join("wait-9").exists(), "its file is removed");
    }

    #[test]
    fn a_stale_slot_is_taken_over() {
        let fake = Fake::live(&[2]);
        let (_dir, slots) = slots(&fake);
        let dir = slots.class_dir(GateClass::Cpu);
        write_slot(&dir, 1, &owner(9, 1).render());
        let view = slots.read(GateClass::Cpu, count(1));
        assert!(
            view.holders.first().is_some_and(|holder| holder.stale),
            "a dead holder is stale"
        );
        assert_eq!(view.used(), 0, "stale holders take no slot");
        let mut ticket = slots
            .enqueue(GateClass::Cpu, count(1), owner(2, 20))
            .unwrap();
        let held = take(&mut ticket).unwrap();
        assert_eq!(held.slot(), 1, "the stale slot is taken over");
        let written =
            Owner::parse(&std::fs::read_to_string(held.path().join(OWNER_FILE)).unwrap()).unwrap();
        assert_eq!(written.pid, 2, "the new owner");
    }

    #[test]
    fn a_pid_taken_over_after_the_owner_began_is_stale() {
        let fake = Fake::live(&[7, 8]);
        let (_dir, slots) = slots(&fake);
        let dir = slots.class_dir(GateClass::Cpu);
        write_slot(&dir, 1, &owner(7, 1_000).render());
        write_slot(&dir, 2, &owner(8, 1_000).render());
        write_slot(
            &dir,
            3,
            &Owner {
                epoch: None,
                ..owner(8, 0)
            }
            .render(),
        );
        fake.started_at(7, 1_000 + headwork_core::gate::PID_REUSE_SLACK_S + 1);
        fake.started_at(8, 990);
        let view = slots.read(GateClass::Cpu, count(3));
        let stale: Vec<(u32, bool)> = view
            .holders
            .iter()
            .map(|holder| (holder.slot, holder.stale))
            .collect();
        assert_eq!(stale, [(1, true), (2, false), (3, false)], "{view:?}");
        fake.started_at(8, 5_000);
        let view = slots.read(GateClass::Cpu, count(3));
        let stale: Vec<(u32, bool)> = view
            .holders
            .iter()
            .map(|holder| (holder.slot, holder.stale))
            .collect();
        assert_eq!(
            stale,
            [(1, true), (2, true), (3, false)],
            "an owner without epoch keeps the plain pid check: {view:?}"
        );
    }

    #[test]
    fn a_held_reap_lock_keeps_a_stale_slot_until_it_is_old() {
        let fake = Fake::live(&[]);
        let (_dir, slots) = slots(&fake);
        let dir = slots.class_dir(GateClass::Cpu);
        let slot = write_slot(&dir, 1, &owner(9, 1).render());
        std::fs::create_dir(dir.join(REAP_LOCK)).unwrap();
        assert!(
            slots.reap(GateClass::Cpu).reaped.is_empty(),
            "another reaper holds the lock"
        );
        assert!(slot.exists(), "the slot stays");
        std::fs::remove_dir(dir.join(REAP_LOCK)).unwrap();
        let reaped = slots.reap(GateClass::Cpu).reaped;
        assert_eq!(reaped.len(), 1, "reaped once the lock is free");
        assert!(!slot.exists(), "the slot is gone");
        assert!(!dir.join(REAP_LOCK).exists(), "the lock is given back");
    }

    /// Keeps a slot folder from being removed while it lives: a file in it opened without sharing on Windows, the
    /// folder read only on unix.
    struct Pinned {
        #[cfg(windows)]
        _file: std::fs::File,
        #[cfg(unix)]
        dir: PathBuf,
    }

    impl Pinned {
        fn new(slot: &Path) -> Self {
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                let file = std::fs::File::options()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .share_mode(0)
                    .open(slot.join("pinned"))
                    .unwrap();
                Self { _file: file }
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(slot, std::fs::Permissions::from_mode(0o555)).unwrap();
                Self {
                    dir: slot.to_path_buf(),
                }
            }
        }
    }

    #[cfg(unix)]
    impl Drop for Pinned {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o755)).ok();
        }
    }

    #[test]
    fn a_removal_that_fails_does_not_stop_the_rest_of_the_reaping() {
        let fake = Fake::live(&[]);
        let (_dir, slots) = slots(&fake);
        let dir = slots.class_dir(GateClass::Cpu);
        let slot = write_slot(&dir, 1, &owner(9, 1).render());
        std::fs::write(dir.join("wait-8"), owner(8, 1).render()).unwrap();
        let pinned = Pinned::new(&slot);
        let reaping = slots.reap(GateClass::Cpu);
        drop(pinned);
        assert!(
            reaping.error.is_some(),
            "the pinned slot's failure is kept: {reaping:?}"
        );
        assert_eq!(reaping.reaped.len(), 1, "{reaping:?}");
        assert!(
            reaping.reaped.first().is_some_and(|gone| gone.waiter),
            "the dead waiter's file is still taken away: {reaping:?}"
        );
        assert!(!dir.join("wait-8").exists(), "its file is gone");
    }

    #[test]
    fn a_young_slot_without_an_owner_is_not_stale() {
        let fake = Fake::live(&[]);
        let (_dir, slots) = slots(&fake);
        let dir = slots.class_dir(GateClass::Gpu);
        std::fs::create_dir_all(dir.join("slot-1")).unwrap();
        let view = slots.read(GateClass::Gpu, count(2));
        assert!(
            view.holders
                .first()
                .is_some_and(|holder| !holder.stale && holder.owner.is_none()),
            "{view:?}"
        );
        assert!(
            slots.reap(GateClass::Gpu).reaped.is_empty(),
            "nothing reaped"
        );
    }

    #[test]
    fn legacy_slots_count_as_cpu() {
        let fake = Fake::live(&[2]);
        let (dir, slots) = slots(&fake);
        let legacy = write_slot(
            dir.path(),
            1,
            "pid=4711\nworktree=S:/Code/headwork/headwork-y\ncommand=check.sh --quick\nstarted=x\n",
        );
        if cfg!(windows) {
            fake.git_bash.lock().unwrap().insert(4711);
        } else {
            fake.native.lock().unwrap().insert(4711);
        }
        let view = slots.read(GateClass::Cpu, count(1));
        assert_eq!(view.used(), 1, "a live legacy holder takes a cpu slot");
        assert!(
            view.wait_message(0).contains("headwork-y check.sh"),
            "{}",
            view.wait_message(0)
        );
        assert_eq!(
            slots.read(GateClass::Gpu, count(1)).used(),
            0,
            "but no gpu slot"
        );
        let mut ticket = slots
            .enqueue(GateClass::Cpu, count(1), owner(2, 20))
            .unwrap();
        assert!(
            take(&mut ticket).is_none(),
            "the legacy holder fills the class"
        );
        if cfg!(windows) {
            fake.git_bash.lock().unwrap().clear();
        } else {
            fake.end(4711);
        }
        assert!(
            take(&mut ticket).is_some(),
            "a gone legacy holder is taken away"
        );
        assert!(!legacy.exists(), "its folder is removed");
    }

    #[test]
    fn a_new_holder_gives_way_to_a_legacy_holder_that_started_past_a_full_class() {
        let fake = Fake::live(&[1, 2, 4711]);
        let (dir, slots) = slots(&fake);
        let mut older = slots
            .enqueue(GateClass::Cpu, count(2), owner(1, 10))
            .unwrap();
        let _older = take(&mut older).unwrap();
        let ticket = slots
            .enqueue(GateClass::Cpu, count(2), owner(2, 20))
            .unwrap();
        let view = slots.read(GateClass::Cpu, count(2));
        let held = ticket.claim(&view, "t", epoch_now() + 10).unwrap().unwrap();
        let path = held.path().to_path_buf();
        write_slot(dir.path(), 1, "pid=4711\nnative=1\ncommand=render\n");
        assert!(
            ticket.settle(held).unwrap().is_none(),
            "the newest holder gives its slot back"
        );
        assert!(!path.exists(), "its folder is gone");
        assert!(
            ticket.wait_file.as_ref().is_some_and(|file| file.exists()),
            "it keeps its place in the queue"
        );
    }

    #[test]
    fn the_older_holder_keeps_its_slot_beside_a_legacy_holder_with_room() {
        let fake = Fake::live(&[2, 4711]);
        let (dir, slots) = slots(&fake);
        let ticket = slots
            .enqueue(GateClass::Cpu, count(2), owner(2, 20))
            .unwrap();
        let view = slots.read(GateClass::Cpu, count(2));
        let held = ticket.claim(&view, "t", 30).unwrap().unwrap();
        write_slot(dir.path(), 1, "pid=4711\nnative=1\ncommand=render\n");
        assert!(
            ticket.settle(held).unwrap().is_some(),
            "one legacy holder and this one fit in two"
        );
    }

    #[test]
    fn contending_holders_never_exceed_the_count() {
        const HOLDERS: u32 = 6;
        let pids: Vec<u32> = (100..100 + HOLDERS).collect();
        let fake = Fake::live(&pids);
        let (_dir, slots) = slots(&fake);
        let inside = Arc::new(AtomicU32::new(0));
        let most = Arc::new(AtomicU32::new(0));
        let threads: Vec<_> = pids
            .into_iter()
            .map(|pid| {
                let slots = slots.clone();
                let inside = Arc::clone(&inside);
                let most = Arc::clone(&most);
                std::thread::spawn(move || {
                    let mut ticket = slots
                        .enqueue(GateClass::Cpu, count(2), owner(pid, u64::from(pid)))
                        .unwrap();
                    let held = loop {
                        if let Some(held) = take(&mut ticket) {
                            break held;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    };
                    let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    inside.fetch_sub(1, Ordering::SeqCst);
                    held.release().unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(
            (1..=2).contains(&most.load(Ordering::SeqCst)),
            "never more than two at once"
        );
        let left = std::fs::read_dir(slots.class_dir(GateClass::Cpu))
            .unwrap()
            .count();
        assert_eq!(left, 0, "every slot and wait file is gone");
    }

    #[test]
    fn the_dir_comes_from_the_environment_then_the_config_then_the_default() {
        let configured = PathBuf::from("configured");
        if std::env::var_os(DIR_VAR).is_none() {
            assert_eq!(
                resolve_dir(Some(configured.clone())),
                Some(configured),
                "configured"
            );
            let default = resolve_dir(None).unwrap();
            assert!(
                default.ends_with("heavy-slots") || default.ends_with(".heavy-slots"),
                "{}",
                default.display()
            );
        }
    }

    #[test]
    fn the_system_judges_this_process_alive() {
        let me = Owner {
            pid: std::process::id(),
            native: true,
            ..Owner::default()
        };
        let mut probe = Probe::new(&SystemLiveness);
        assert!(probe.alive(&me), "this process runs");
    }
}
