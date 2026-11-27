//! Remote Control leftovers: ended sessions the Claude app may still list.
//!
//! These are the sessions that did not leave through `/exit`, which only the user can archive there
//! (`docs/decisions/2026-11-27-rc-leftovers.md`).
//!
//! `headwork leftovers` lists them, `headwork leftovers clear` records that the user archived them, and `doctor` and
//! `board` count them. Nothing here touches a process or the app.

use anyhow::{Result, bail};
use headwork_core::Config;
use headwork_core::leftover::{Leftover, select, window_seconds};
use headwork_store::Store;

/// The leftovers not yet archived: those of the window `[watch] leftover_days` sets, or of all time with `all`.
///
/// # Errors
///
/// Returns the store's failure.
pub fn list(store: &Store, config: &Config, all: bool) -> Result<Vec<Leftover>> {
    let since = match window_seconds(config.watch.leftover_days) {
        Some(seconds) if !all => Some(store.instant_ago(seconds)?),
        Some(_) | None => None,
    };
    Ok(select(&store.ended_session_facts()?, since.as_deref()))
}

/// How many leftovers the window holds.
///
/// # Errors
///
/// Returns the store's failure.
pub fn count(store: &Store, config: &Config) -> Result<usize> {
    Ok(list(store, config, false)?.len())
}

/// Records that the user archived leftovers: with `everything`, every unarchived leftover of all time, else those whose
/// Remote Control name or session id is in `names`. Returns how many it marked.
///
/// # Errors
///
/// Fails when a name matches no leftover, or on the store's failure.
pub fn clear(store: &mut Store, names: &[String], everything: bool) -> Result<usize> {
    let found = select(&store.ended_session_facts()?, None);
    let sessions: Vec<String> = if everything {
        found.into_iter().map(|leftover| leftover.session).collect()
    } else {
        if let Some(missing) = names.iter().find(|name| {
            !found
                .iter()
                .any(|leftover| &leftover.rc_name == *name || &leftover.session == *name)
        }) {
            bail!("no leftover named `{missing}`: see `headwork leftovers --all`");
        }
        found
            .into_iter()
            .filter(|leftover| {
                names
                    .iter()
                    .any(|name| &leftover.rc_name == name || &leftover.session == name)
            })
            .map(|leftover| leftover.session)
            .collect()
    };
    Ok(store.mark_rc_archived(&sessions)?)
}
