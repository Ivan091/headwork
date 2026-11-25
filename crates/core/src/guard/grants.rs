//! Paths outside its folder a worker may write, and the git commands it may run on them.
//!
//! The grants are its project's `extra_writable` for its task kind, and the files its brief lists on a `Writes:` line
//! under `hooks.brief_writable`. A worker may edit them, and run a narrow set of git commands in the repository that
//! holds them: `add` and `rm` of those paths, `commit` of what is staged or of those paths, `push` without force or
//! deletion, and `pull`. Such a repository is a notes repository committed on its default branch by design, so the
//! default-branch and push-repository rules do not apply there.

use super::paths::{GuardPath, Places};

/// The word that starts a brief line listing the files the worker may write outside its folder.
pub const WRITES_MARK: &str = "writes:";

/// Characters that wrap or end a path on a `Writes:` line without being part of it.
const PATH_WRAPPERS: &[char] = &[
    '`', '"', '\'', '(', ')', '[', ']', '<', '>', ',', ';', '.', ':',
];

/// `git commit` options that take the next word as their value.
const COMMIT_VALUE_OPTIONS: &[&str] = &[
    "--message",
    "--file",
    "--author",
    "--date",
    "--reuse-message",
    "--reedit-message",
    "--fixup",
    "--squash",
    "--template",
    "--trailer",
    "--cleanup",
    "--gpg-sign",
];

/// Short `git commit` options that take a value: the rest of the word, or the next word.
const COMMIT_VALUE_LETTERS: &[char] = &['m', 'F', 'C', 'c', 't'];

/// `git commit` options that would take in more than the named or staged paths, or rewrite an earlier commit.
const COMMIT_WIDE: &[&str] = &[
    "-a",
    "--all",
    "--amend",
    "--pathspec-from-file",
    "--interactive",
    "--patch",
    "-p",
];

/// `git push` options that delete or push more than the current branch.
const PUSH_WIDE: &[&str] = &["--delete", "--mirror", "--all", "--prune", "--tags"];

/// How a `git push` forces the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ForceKind {
    /// No force.
    None,
    /// `--force-with-lease` or `--force-if-includes`, without a plain force.
    Lease,
    /// `--force`, `-f` (also inside a group of short flags) or a `+refspec`.
    Plain,
}

/// What a `git push` command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PushForm<'a> {
    /// How it forces.
    pub force: ForceKind,
    /// It deletes or pushes more than one branch (`--all`, `--mirror`, `--tags`, `--prune`, `--delete`, `-d`).
    pub wide: bool,
    /// The remote and then the refspecs, as written.
    pub positional: Vec<&'a str>,
}

/// Reads a `git push` command line once for every rule that looks at it.
pub(super) fn classify_push(args: &[String]) -> PushForm<'_> {
    let mut plain = false;
    let mut lease = false;
    let mut wide = false;
    let mut positional = Vec::new();
    for word in args {
        let name = word.split('=').next().unwrap_or(word);
        if let Some(letters) = word
            .strip_prefix('-')
            .filter(|letters| !letters.starts_with('-'))
        {
            plain |= letters.contains('f');
            wide |= letters.contains('d');
        } else if word.starts_with('-') {
            plain |= name == "--force";
            lease |= matches!(name, "--force-with-lease" | "--force-if-includes");
            wide |= PUSH_WIDE.contains(&name);
        } else {
            plain |= word.starts_with('+');
            positional.push(word.as_str());
        }
    }
    let force = if plain {
        ForceKind::Plain
    } else if lease {
        ForceKind::Lease
    } else {
        ForceKind::None
    };
    PushForm {
        force,
        wide,
        positional,
    }
}

/// The paths a brief lists on its `Writes:` lines, as written: each word after the mark, without backticks, quotes and
/// trailing punctuation. A line may start with a list marker.
#[must_use]
pub fn brief_writes(brief: &str) -> Vec<&str> {
    brief
        .lines()
        .filter_map(|line| {
            let line = line
                .trim_start()
                .trim_start_matches(['-', '*', '+'])
                .trim_start();
            let head = line.get(..WRITES_MARK.len())?;
            head.eq_ignore_ascii_case(WRITES_MARK)
                .then(|| line.get(WRITES_MARK.len()..))
                .flatten()
        })
        .flat_map(|rest| rest.split(|c: char| c.is_whitespace() || c == ','))
        .map(|word| word.trim_matches(PATH_WRAPPERS))
        .filter(|word| !word.is_empty())
        .collect()
}

/// The paths a brief grants: those its `Writes:` lines list that are absolute and lie strictly below one of `roots`.
#[must_use]
pub fn brief_grants(brief: &str, roots: &[GuardPath], places: &Places) -> Vec<GuardPath> {
    let mut grants: Vec<GuardPath> = brief_writes(brief)
        .into_iter()
        .filter_map(|word| GuardPath::absolute(&places.expand(word)))
        .filter(|path| {
            roots
                .iter()
                .any(|root| path != root && path.is_within(root))
        })
        .collect();
    grants.sort();
    grants.dedup();
    grants
}

/// How the grants judge a git write in `folder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GrantedGit {
    /// No grant lies in or above `folder`: the grants say nothing.
    Unrelated,
    /// A git write the grants allow.
    Allowed,
    /// A git write in a granted repository that reaches beyond the grants, and why.
    Beyond(String),
}

/// Judges the git write `sub` with `args` in `folder` against the grants. A grant relates to `folder` when it lies in
/// it or `folder` lies in it, so `git -C <repo root>` and a `cd` into the granted folder both count.
pub(super) fn granted_git(
    grants: &[GuardPath],
    places: &Places,
    folder: &GuardPath,
    sub: &str,
    args: &[String],
) -> GrantedGit {
    if !grants
        .iter()
        .any(|grant| grant.is_within(folder) || folder.is_within(grant))
    {
        return GrantedGit::Unrelated;
    }
    let verdict = match sub {
        "add" | "rm" => pathspecs_of(args).and_then(|paths| {
            if paths.is_empty() {
                Err("name the files: without paths it takes in the whole repository".to_owned())
            } else {
                within_grants(grants, places, folder, &paths)
            }
        }),
        "commit" => {
            commit_pathspecs(args).and_then(|paths| within_grants(grants, places, folder, &paths))
        }
        "push" => check_push(args),
        "pull" => Ok(()),
        _ => Err(
            "only `add`, `rm`, `commit`, `push` and `pull` of these paths are allowed there"
                .to_owned(),
        ),
    };
    match verdict {
        Ok(()) => GrantedGit::Allowed,
        Err(why) => GrantedGit::Beyond(why),
    }
}

/// The paths named by `add` or `rm`: every word that is not an option, and every word after `--`.
fn pathspecs_of(args: &[String]) -> Result<Vec<&str>, String> {
    let mut paths = Vec::new();
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        if word == "--" {
            paths.extend(words.by_ref());
            break;
        }
        if word.starts_with("--pathspec-from-file") {
            return Err("name the files on the command line, not in a file".to_owned());
        }
        if !word.starts_with('-') {
            paths.push(word);
        }
    }
    Ok(paths)
}

/// The paths a `commit` names, skipping option values; refuses `-a`, `--amend` and other options that commit more.
fn commit_pathspecs(args: &[String]) -> Result<Vec<&str>, String> {
    let wide = |word: &str| {
        Err(format!(
            "`git commit {word}` would commit more than these paths"
        ))
    };
    let mut paths = Vec::new();
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        if word == "--" {
            paths.extend(words.by_ref());
            break;
        }
        if let Some(long) = word.strip_prefix("--") {
            let name = long.split('=').next().unwrap_or(long);
            let option = format!("--{name}");
            if COMMIT_WIDE.contains(&option.as_str()) {
                return wide(word);
            }
            if !long.contains('=') && COMMIT_VALUE_OPTIONS.contains(&option.as_str()) {
                words.next();
            }
            continue;
        }
        if let Some(letters) = word.strip_prefix('-').filter(|letters| !letters.is_empty()) {
            for (at, letter) in letters.char_indices() {
                if matches!(letter, 'a' | 'p') {
                    return wide(word);
                }
                if COMMIT_VALUE_LETTERS.contains(&letter) {
                    if at + letter.len_utf8() == letters.len() {
                        words.next();
                    }
                    break;
                }
            }
            continue;
        }
        paths.push(word);
    }
    Ok(paths)
}

/// Whether every path, resolved in `folder`, lies within a grant. Pathspec magic (`:/`, `:(top)`) is refused, since it
/// reaches the whole repository.
fn within_grants(
    grants: &[GuardPath],
    places: &Places,
    folder: &GuardPath,
    paths: &[&str],
) -> Result<(), String> {
    for path in paths {
        if path.starts_with(':') {
            return Err(format!(
                "`{path}` is pathspec magic; name the files by their paths"
            ));
        }
        let target = places.resolve(folder.as_str(), path);
        if !grants.iter().any(|grant| target.is_within(grant)) {
            let granted: Vec<&str> = grants.iter().map(GuardPath::as_str).collect();
            return Err(format!(
                "{target} is not one of the paths this task may write; name only these or files below them: {}",
                granted.join(", ")
            ));
        }
    }
    Ok(())
}

/// A push without force, deletion or a mirror.
fn check_push(args: &[String]) -> Result<(), String> {
    let form = classify_push(args);
    let deletes = form.positional.iter().any(|word| word.starts_with(':'));
    if form.force != ForceKind::None || form.wide || deletes {
        return Err(format!(
            "`git push {}` forces, deletes or pushes more than the branch",
            args.join(" ")
        ));
    }
    Ok(())
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

    #[test]
    fn writes_lines_list_paths() {
        let brief = "# Audit\n\nRead `S:/Code/notes/audit-2026-11-15.md` first.\n\nWrites: `S:/Code/notes/plan.md`, \
                     S:\\Code\\notes\\b.md.\n- writes: /s/Code/notes/c.md and `~/notes/d.md`\nWritesnot: S:/Code/notes/e.md\n";
        assert_eq!(
            brief_writes(brief),
            vec![
                "S:/Code/notes/plan.md",
                r"S:\Code\notes\b.md",
                "/s/Code/notes/c.md",
                "and",
                "~/notes/d.md"
            ],
            "only Writes: lines, words unwrapped"
        );
    }

    #[test]
    fn a_brief_grants_only_absolute_paths_strictly_below_a_root() {
        let brief = "Writes: `S:/Code/notes/plan.md` S:/Code/notes/ S:/Code/todo.md notes/rel.md /s/code/notes/sub/x.md \
                     S:/Code/notes/../briefs/a.md ~/notes/d.md\nRead S:/Code/notes/other.md.";
        let roots = [
            GuardPath::new("S:/Code/notes"),
            GuardPath::new("C:/Users/User/notes"),
        ];
        let grants: Vec<String> = brief_grants(brief, &roots, &places())
            .iter()
            .map(|grant| grant.as_str().to_owned())
            .collect();
        assert_eq!(
            grants,
            vec![
                "c:/users/user/notes/d.md",
                "s:/code/notes/plan.md",
                "s:/code/notes/sub/x.md"
            ],
            "the root itself, other folders, relative paths and mere mentions grant nothing"
        );
        assert!(
            brief_grants(brief, &[], &places()).is_empty(),
            "no roots, no grants"
        );
    }

    #[test]
    fn commit_options_with_values_are_not_paths() {
        let args = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let cases: &[(&str, Result<Vec<&str>, ()>)] = &[
            ("-q -m x", Ok(vec![])),
            ("-qm x a.md", Ok(vec!["a.md"])),
            ("-mx a.md", Ok(vec!["a.md"])),
            ("--message x -- b.md", Ok(vec!["b.md"])),
            ("--message=x c.md", Ok(vec!["c.md"])),
            ("-F msg.txt", Ok(vec![])),
            ("--author A -m x", Ok(vec![])),
            ("-am x", Err(())),
            ("-qa", Err(())),
            ("--all", Err(())),
            ("--amend", Err(())),
            ("-p", Err(())),
        ];
        for (line, want) in cases {
            let words = args(line);
            let got = commit_pathspecs(&words).map_err(|_| ());
            assert_eq!(got, *want, "{line}");
        }
    }
}
