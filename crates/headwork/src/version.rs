//! Which build this is: the release version and commit a release build is given, and the store schema it knows
//! (`docs/autoupdate.md`, "Versions").
//!
//! The release workflow builds with `HEADWORK_VERSION` (`0.1.<run>`) and `HEADWORK_COMMIT` (the built commit) in the
//! environment; any other build has neither and calls itself `dev`. The package version stays `0.1.0` across builds.

use headwork_store::MIGRATIONS;

/// The release version this build was given, when it is a release build.
#[must_use]
pub fn release() -> Option<&'static str> {
    given(option_env!("HEADWORK_VERSION"))
}

/// The commit this build was made from, when it is a release build.
#[must_use]
pub fn commit() -> Option<&'static str> {
    given(option_env!("HEADWORK_COMMIT"))
}

/// What `--version` prints after the program's name: `0.1.214 (<commit>, schema 14)`, or `0.1.0 (dev, schema 14)`
/// for a build the release workflow did not make.
#[must_use]
pub fn text() -> String {
    format!(
        "{} ({}, schema {})",
        release().unwrap_or(env!("CARGO_PKG_VERSION")),
        commit().unwrap_or("dev"),
        MIGRATIONS.len()
    )
}

fn given(value: Option<&'static str>) -> Option<&'static str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_names_the_version_the_commit_and_the_schema() {
        let text = text();
        assert!(
            text.starts_with(release().unwrap_or(env!("CARGO_PKG_VERSION"))),
            "{text}"
        );
        assert!(text.contains(commit().unwrap_or("dev")), "{text}");
        assert!(
            text.ends_with(&format!(", schema {})", MIGRATIONS.len())),
            "{text}"
        );
    }

    #[test]
    fn blank_values_count_as_absent() {
        assert_eq!(given(None), None, "unset");
        assert_eq!(given(Some("")), None, "empty");
        assert_eq!(given(Some("  ")), None, "blank");
        assert_eq!(given(Some("0.1.7")), Some("0.1.7"), "set");
    }
}
