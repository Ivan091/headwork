//! Just enough shell parsing to see which programs a Bash command runs.
//!
//! The parser splits a command line into simple commands at `;`, `&`, `|`, newlines, parentheses and backticks outside
//! quotes, and each simple command into words with quotes and backslashes removed. It strips leading assignments and
//! wrappers such as `env`, `sudo` and `rtk`, and descends into `bash -c "<script>"`. It is a pattern matcher for
//! hooks, not a shell: anything it cannot see stays allowed.

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
        let program = self.program().replace('\\', "/");
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
    /// its words, normalized as in [`SimpleCommand::text`], contain the pattern's words in order and whole. The
    /// pattern's first word also matches a longer path ending in it, so an absolute script path counts.
    #[must_use]
    pub fn runs(&self, pattern: &str) -> bool {
        let wanted: Vec<String> = pattern.split_whitespace().map(normalize_word).collect();
        let words: Vec<String> = self.words.iter().map(|word| normalize_word(word)).collect();
        if wanted.is_empty() {
            return false;
        }
        (0..words.len()).any(|start| {
            wanted.iter().enumerate().all(|(offset, want)| {
                words.get(start + offset).is_some_and(|word| {
                    word == want
                        || (offset == 0
                            && want.contains('/')
                            && word.ends_with(&format!("/{want}")))
                })
            })
        })
    }
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
                }
                ';' | '&' | '|' | '\n' | '(' | ')' | '`' | '{' | '}' => {
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
}
