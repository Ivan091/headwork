//! Reading `git worktree add` out of a Bash command line, so the worktrees a worker makes itself can be recorded as its
//! own.

use super::paths::Places;
use super::shell::{SimpleCommand, commands};
use super::{GitLine, git_line};

/// A worktree one `git worktree add` in a command line makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeAdd {
    /// The folder, absolute with forward slashes and the case as written.
    pub dir: String,
    /// The branch it checks out: `-b` or `-B`, else the commit-ish, else the folder's name.
    pub branch: String,
}

/// The worktrees the `git worktree add` commands of `line` make, run from `cwd`.
///
/// It follows `cd` and `pushd` and each `-C`, so a relative folder is resolved where git resolves it; a command whose
/// folder cannot be told (`cd -`, `popd`, `--git-dir`) yields nothing. Whether the command ran, and made a
/// worktree, is for the caller to check.
#[must_use]
pub fn worktree_adds(line: &str, cwd: &str, places: &Places) -> Vec<WorktreeAdd> {
    let mut here = Some(cwd.to_owned());
    let mut found = Vec::new();
    for command in commands(line) {
        match command.program_name().as_str() {
            "cd" | "pushd" => {
                here = here.and_then(|dir| {
                    let target = command
                        .args()
                        .iter()
                        .map(String::as_str)
                        .find(|arg| *arg == "-" || !arg.starts_with('-'))
                        .unwrap_or("~");
                    (target != "-").then(|| places.resolve_exact(&dir, target))
                });
            }
            "popd" => here = None,
            "git" => {
                if let Some(dir) = &here
                    && let Some(added) = added_by(&command, dir, places)
                {
                    found.push(added);
                }
            }
            _ => {}
        }
    }
    found
}

fn added_by(command: &SimpleCommand, cwd: &str, places: &Places) -> Option<WorktreeAdd> {
    let GitLine { sub, args, .. } = git_line(command)?;
    if sub != "worktree" || args.first().map(String::as_str) != Some("add") {
        return None;
    }
    let folder = git_folder(command, cwd, places)?;
    let mut branch = None;
    let mut positional = Vec::new();
    let mut detached = false;
    let mut words = args.iter().skip(1);
    while let Some(word) = words.next() {
        match word.as_str() {
            "-b" | "-B" => branch = Some(words.next()?.clone()),
            "--reason" => {
                words.next();
            }
            "-d" | "--detach" => detached = true,
            flag if flag.starts_with('-') => {}
            other => positional.push(other),
        }
    }
    let path = positional.first()?;
    let dir = places.resolve_exact(&folder, path);
    let named = || {
        dir.rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    };
    let branch = branch
        .or_else(|| {
            positional
                .get(1)
                .filter(|_| !detached)
                .map(|commit| (*commit).to_owned())
        })
        .or_else(named)?;
    Some(WorktreeAdd { dir, branch })
}

fn git_folder(command: &SimpleCommand, cwd: &str, places: &Places) -> Option<String> {
    let mut folder = cwd.to_owned();
    let mut words = command.args().iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "-C" => folder = places.resolve_exact(&folder, words.next()?),
            "-c" | "--namespace" => {
                words.next();
            }
            flag if flag.starts_with("--git-dir") || flag.starts_with("--work-tree") => {
                return None;
            }
            flag if flag.starts_with('-') => {}
            _ => break,
        }
    }
    Some(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places() -> Places {
        Places {
            home: Some("C:/Users/User".to_owned()),
            temp: None,
        }
    }

    fn made(line: &str, cwd: &str) -> Vec<(String, String)> {
        worktree_adds(line, cwd, &places())
            .into_iter()
            .map(|added| (added.dir, added.branch))
            .collect()
    }

    type Made = Vec<(String, String)>;

    fn pair(dir: &str, branch: &str) -> Vec<(String, String)> {
        vec![(dir.to_owned(), branch.to_owned())]
    }

    #[test]
    fn every_form_of_add_is_read() {
        let main = "S:/Code/me-studio/me-studio";
        let table: &[(&str, &str, Made)] = &[
            (
                "git worktree add -b feat ../me-studio-feat origin/master",
                main,
                pair("S:/Code/me-studio/me-studio-feat", "feat"),
            ),
            (
                "git worktree add -B feat S:/Code/x/Wt",
                main,
                pair("S:/Code/x/Wt", "feat"),
            ),
            (
                "git worktree add --detach ../wt-timing-probe HEAD",
                "S:/Code/me-sim/me-sim",
                pair("S:/Code/me-sim/wt-timing-probe", "wt-timing-probe"),
            ),
            (
                "git worktree add ../wt origin/topic",
                main,
                pair("S:/Code/me-studio/wt", "origin/topic"),
            ),
            (
                "git -C S:/Code/me-studio/me-studio worktree add ../wt -b a",
                "S:/elsewhere",
                pair("S:/Code/me-studio/wt", "a"),
            ),
            (
                "cd ../other && git worktree add --lock --reason x sub",
                main,
                pair("S:/Code/me-studio/other/sub", "sub"),
            ),
            (
                "git worktree add -f ~/wt-home",
                main,
                pair("C:/Users/User/wt-home", "wt-home"),
            ),
            (
                "cd 'S:\\Code\\me-sim\\me-sim' && git worktree add '..\\wt-win'",
                main,
                pair("S:/Code/me-sim/wt-win", "wt-win"),
            ),
        ];
        for (line, cwd, want) in table {
            assert_eq!(&made(line, cwd), want, "{line}");
        }
    }

    #[test]
    fn other_commands_and_unknown_folders_make_nothing() {
        let cwd = "S:/Code/x/x";
        for line in [
            "git worktree list",
            "git worktree remove ../wt",
            "git worktree prune",
            "git worktree add",
            "git add .",
            "echo git worktree add ../wt",
            "cd - && git worktree add ../wt",
            "popd; git worktree add ../wt",
            "git --git-dir=.git worktree add ../wt",
        ] {
            assert!(made(line, cwd).is_empty(), "{line}");
        }
    }

    #[test]
    fn a_line_may_add_several() {
        let both = made(
            "git worktree add ../a && git worktree add -b b ../b",
            "S:/Code/x/x",
        );
        assert_eq!(
            both,
            [
                ("S:/Code/x/a".to_owned(), "a".to_owned()),
                ("S:/Code/x/b".to_owned(), "b".to_owned())
            ]
        );
    }
}
