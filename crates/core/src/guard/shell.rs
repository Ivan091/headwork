//! Just enough shell parsing to see which programs a Bash command runs.
//!
//! The parser splits a command line into simple commands at `;`, `&`, `|`, newlines, parentheses and backticks outside
//! quotes, and each simple command into words with quotes and backslashes removed. It strips leading assignments and
//! wrappers such as `env`, `sudo` and `rtk`, and descends into `bash -c "<script>"`. Here-document bodies are data,
//! not commands, and are skipped up to their terminator. It is a pattern matcher for
//! hooks, not a shell: anything it cannot see stays allowed.

use std::iter::Peekable;
use std::str::Chars;

/// Programs that only run the rest of their words.
const WRAPPERS: &[&str] = &["env", "sudo", "command", "exec", "nohup", "time", "rtk"];

/// Wrapper options that take a value, such as `env -u NAME` and `sudo -u user`.
const WITH_VALUE: &[&str] = &["-u", "--unset", "-C", "--chdir", "-g", "--user", "--group"];

/// The words `rtk` puts before the real command.
const RTK_VERBS: &[&str] = &["bypass", "proxy"];

/// Shells whose `-c` argument is parsed again, and whose script argument becomes the program.
const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash"];

/// One simple command: the program first, then its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleCommand {
    words: Vec<String>,
}

impl SimpleCommand {
    /// The program as written, such as `./scripts/ship.sh`.
    #[must_use]
    pub fn program(&self) -> &str {
        self.words.first().map_or("", String::as_str)
    }

    /// The program's base name, lower case, without `.exe` or `.cmd`.
    #[must_use]
    pub fn program_name(&self) -> String {
        base_name(self.program())
    }

    /// The arguments after the program.
    #[must_use]
    pub fn args(&self) -> &[String] {
        self.words.get(1..).unwrap_or_default()
    }

    /// The words joined by single spaces, slashes forward and a leading `./` dropped from each word, for matching
    /// configured command patterns.
    #[must_use]
    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|word| normalize_word(word))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether the command runs a configured pattern such as `scripts/ship.sh` or `cargo run --release -p render`:
    /// its words, normalized as in [`SimpleCommand::text`], start with the pattern's words, whole. The pattern's first
    /// word also matches a longer path ending in it, so an absolute script path counts; a first word without a slash
    /// matches the program's base name under any folder or Windows extension, so `me-studio publish` matches
    /// `target/release/me-studio.exe publish`. Only the program position counts, so `cat scripts/publish-now.sh` does
    /// not run it; [`commands`] already makes the script argument of `bash script.sh` the program.
    #[must_use]
    pub fn runs(&self, pattern: &str) -> bool {
        let wanted: Vec<String> = pattern.split_whitespace().map(normalize_word).collect();
        if wanted.is_empty() {
            return false;
        }
        wanted.iter().enumerate().all(|(offset, want)| {
            self.words
                .get(offset)
                .map(|word| normalize_word(word))
                .is_some_and(|word| {
                    word == *want
                        || (offset == 0
                            && if want.contains('/') {
                                word.ends_with(&format!("/{want}"))
                            } else {
                                base_name(&word) == base_name(want)
                            })
                })
        })
    }
}

/// A program's name without its folder or a Windows extension, lower case.
fn base_name(program: &str) -> String {
    let program = program.replace('\\', "/");
    let base = program
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_lowercase();
    [".exe", ".cmd", ".bat"]
        .iter()
        .find_map(|ext| base.strip_suffix(ext))
        .map_or_else(|| base.clone(), str::to_owned)
}

fn normalize_word(word: &str) -> String {
    let word = word.replace('\\', "/");
    word.strip_prefix("./")
        .map_or_else(|| word.clone(), str::to_owned)
}

/// Every simple command in a Bash command line, including those inside `bash -c`.
#[must_use]
pub fn commands(line: &str) -> Vec<SimpleCommand> {
    let mut out = Vec::new();
    collect(line, &mut out, 0);
    out
}

fn collect(line: &str, out: &mut Vec<SimpleCommand>, depth: u8) {
    for words in split(line) {
        let words = unwrap(&words);
        if words.is_empty() {
            continue;
        }
        let name = SimpleCommand {
            words: words.clone(),
        }
        .program_name();
        if SHELLS.contains(&name.as_str()) {
            if let Some(index) = words.iter().position(|word| word == "-c") {
                if depth < 4
                    && let Some(script) = words.get(index + 1)
                {
                    collect(script, out, depth + 1);
                }
                continue;
            }
            let script: Vec<String> = words
                .iter()
                .skip(1)
                .skip_while(|word| word.starts_with('-'))
                .cloned()
                .collect();
            if !script.is_empty() {
                out.push(SimpleCommand { words: script });
                continue;
            }
        }
        out.push(SimpleCommand { words });
    }
}

fn unwrap(words: &[String]) -> Vec<String> {
    let mut rest = words;
    loop {
        match rest.split_first() {
            Some((first, tail)) if is_assignment(first) => rest = tail,
            Some((first, tail)) if WRAPPERS.contains(&first.to_lowercase().as_str()) => {
                rest = tail;
                if first == "rtk"
                    && let Some((verb, after)) = rest.split_first()
                    && RTK_VERBS.contains(&verb.as_str())
                {
                    rest = after;
                }
                while let Some((flag, after)) = rest.split_first() {
                    if WITH_VALUE.contains(&flag.as_str()) {
                        rest = after.get(1..).unwrap_or_default();
                    } else if flag.starts_with('-') || is_assignment(flag) {
                        rest = after;
                    } else {
                        break;
                    }
                }
            }
            _ => return rest.to_vec(),
        }
    }
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        let mut chars = name.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

/// Splits a line into simple commands, each a list of words.
fn split(line: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = Quote::None;
    let mut heredocs: Vec<Heredoc> = Vec::new();
    let mut chars = line.chars().peekable();
    let end_word = |words: &mut Vec<String>, word: &mut String, in_word: &mut bool| {
        if *in_word {
            words.push(std::mem::take(word));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match quote {
            Quote::Single => {
                if c == '\'' {
                    quote = Quote::None;
                } else {
                    word.push(c);
                }
            }
            Quote::Double => match c {
                '"' => quote = Quote::None,
                '\\' => match chars.peek() {
                    Some(&next @ ('"' | '\\' | '$' | '`')) => {
                        word.push(next);
                        chars.next();
                    }
                    _ => word.push(c),
                },
                _ => word.push(c),
            },
            Quote::None => match c {
                '\'' => {
                    quote = Quote::Single;
                    in_word = true;
                }
                '"' => {
                    quote = Quote::Double;
                    in_word = true;
                }
                '\\' => {
                    if let Some(next) = chars.next()
                        && next != '\n'
                    {
                        word.push(next);
                        in_word = true;
                    }
                }
                '#' if !in_word => {
                    for skipped in chars.by_ref() {
                        if skipped == '\n' {
                            break;
                        }
                    }
                    end_word(&mut words, &mut word, &mut in_word);
                    commands.push(std::mem::take(&mut words));
                    skip_heredocs(&mut chars, &mut heredocs);
                }
                '<' if chars.next_if_eq(&'<').is_some() => {
                    end_word(&mut words, &mut word, &mut in_word);
                    heredocs.extend(heredoc_start(&mut chars));
                }
                '\n' => {
                    end_word(&mut words, &mut word, &mut in_word);
                    commands.push(std::mem::take(&mut words));
                    skip_heredocs(&mut chars, &mut heredocs);
                }
                ';' | '&' | '|' | '(' | ')' | '`' | '{' | '}' => {
                    end_word(&mut words, &mut word, &mut in_word);
                    commands.push(std::mem::take(&mut words));
                }
                c if c.is_whitespace() => end_word(&mut words, &mut word, &mut in_word),
                c => {
                    word.push(c);
                    in_word = true;
                }
            },
        }
    }
    end_word(&mut words, &mut word, &mut in_word);
    commands.push(words);
    commands
        .into_iter()
        .map(|words| {
            words
                .into_iter()
                .filter(|word| word != "$")
                .collect::<Vec<_>>()
        })
        .filter(|words| !words.is_empty())
        .collect()
}

/// A here-document whose body starts at the next line.
#[derive(Debug)]
struct Heredoc {
    terminator: String,
    strip_tabs: bool,
}

/// Reads what follows `<<`: a here-document's `-` and terminator, or nothing for a here-string `<<<`.
fn heredoc_start(chars: &mut Peekable<Chars<'_>>) -> Option<Heredoc> {
    if chars.next_if_eq(&'<').is_some() {
        return None;
    }
    let strip_tabs = chars.next_if_eq(&'-').is_some();
    heredoc_terminator(chars).map(|terminator| Heredoc {
        terminator,
        strip_tabs,
    })
}

/// Reads the word after `<<` or `<<-`, quotes removed.
fn heredoc_terminator(chars: &mut Peekable<Chars<'_>>) -> Option<String> {
    while chars.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
    let mut word = String::new();
    let mut quote = Quote::None;
    while let Some(&c) = chars.peek() {
        match (quote, c) {
            (Quote::None, '\'') => quote = Quote::Single,
            (Quote::None, '"') => quote = Quote::Double,
            (Quote::Single, '\'') | (Quote::Double, '"') => quote = Quote::None,
            (Quote::None, '\\') => {
                chars.next();
                if let Some(next) = chars.peek() {
                    word.push(*next);
                }
            }
            (Quote::None, c) if c.is_whitespace() || ";&|()<>`".contains(c) => break,
            (_, c) => word.push(c),
        }
        chars.next();
    }
    (!word.is_empty()).then_some(word)
}

/// Skips the bodies of the pending here-documents, each up to its terminator line.
fn skip_heredocs(chars: &mut Peekable<Chars<'_>>, heredocs: &mut Vec<Heredoc>) {
    for heredoc in heredocs.drain(..) {
        loop {
            let mut line = String::new();
            let mut ended = true;
            for c in chars.by_ref() {
                if c == '\n' {
                    ended = false;
                    break;
                }
                line.push(c);
            }
            let line = line.trim_end_matches('\r');
            let line = if heredoc.strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line
            };
            if line == heredoc.terminator {
                break;
            }
            if ended {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn programs(line: &str) -> Vec<String> {
        commands(line).iter().map(SimpleCommand::text).collect()
    }

    #[test]
    fn splits_at_operators_outside_quotes() {
        assert_eq!(
            programs("cd x && git commit -m 'a; b' || echo \"c | d\"; ls"),
            ["cd x", "git commit -m a; b", "echo c | d", "ls"],
            "operators inside quotes stay"
        );
    }

    #[test]
    fn strips_wrappers_and_assignments() {
        assert_eq!(
            programs("FOO=1 rtk bypass env -u X BAR=2 git push"),
            ["git push"],
            "wrappers go"
        );
    }

    #[test]
    fn descends_into_bash_c_and_substitutions() {
        assert_eq!(
            programs("bash -c \"cargo build && git push\"; echo $(git commit -m x)"),
            ["cargo build", "git push", "echo", "git commit -m x"],
            "nested commands are seen"
        );
    }

    #[test]
    fn a_shell_script_argument_becomes_the_program() {
        let found = commands("bash ./scripts/ship.sh -m 'feat: x'");
        assert_eq!(found.len(), 1, "one command");
        assert_eq!(
            found.first().map(SimpleCommand::program),
            Some("./scripts/ship.sh"),
            "the script"
        );
    }

    #[test]
    fn comments_are_skipped() {
        assert_eq!(
            programs("ls # git push\ncargo test"),
            ["ls", "cargo test"],
            "comment dropped"
        );
    }

    #[test]
    fn patterns_match_whole_words() {
        let command = commands("./scripts/publish-now.sh --all").remove(0);
        assert!(command.runs("scripts/publish-now.sh"), "relative script");
        assert!(!command.runs("scripts/publish.sh"), "other script");
        let absolute = commands("S:/Code/me-sim/me-sim/scripts/publish-now.sh").remove(0);
        assert!(absolute.runs("scripts/publish-now.sh"), "absolute script");
        let render = commands("cargo run --release -p render -- scene").remove(0);
        assert!(
            render.runs("cargo run --release -p render"),
            "multi-word pattern"
        );
        assert!(!render.runs("cargo run --release -p rend"), "partial word");
    }

    #[test]
    fn program_names_drop_extensions() {
        let command = commands("'C:\\tools\\Cargo.EXE' build").remove(0);
        assert_eq!(command.program_name(), "cargo", "base name");
    }

    #[test]
    fn heredoc_bodies_are_not_commands() {
        assert_eq!(
            programs("cat <<'EOF' > notes.md\ncargo build --release\ngit push\nEOF\nls"),
            ["cat > notes.md", "ls"],
            "the body is data"
        );
        assert_eq!(
            programs("cat <<-END\n\tnpm install\n\tEND\ncargo test"),
            ["cat", "cargo test"],
            "a tab-stripped terminator ends it"
        );
        assert_eq!(
            programs("python - <<PY && echo done\nimport os\nPY\nwc -l x"),
            ["python -", "echo done", "wc -l x"],
            "commands after the operator on the first line stay"
        );
        assert_eq!(
            programs("cat <<<\"cargo build\""),
            ["cat cargo build"],
            "a here-string is not a here-document"
        );
    }

    #[test]
    fn patterns_match_at_the_program_position_only() {
        for line in [
            "cat scripts/publish-now.sh",
            "wc -l ./scripts/publish-now.sh",
            "grep -n publish scripts/publish-now.sh",
        ] {
            assert!(
                commands(line)
                    .iter()
                    .all(|command| !command.runs("scripts/publish-now.sh")),
                "{line} only reads the script"
            );
        }
        for line in [
            "bash scripts/publish-now.sh",
            "sh -x ./scripts/publish-now.sh --all",
        ] {
            assert!(
                commands(line)
                    .iter()
                    .any(|command| command.runs("scripts/publish-now.sh")),
                "{line} runs the script"
            );
        }
    }
}
