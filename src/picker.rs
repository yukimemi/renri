//! Interactive fallback for "give me one of these worktrees" prompts.
//!
//! Uses `inquire`'s fuzzy `Select`. The picker writes its TUI to stderr,
//! so stdout stays clean for `cd "$(renri cd foo)"`-style shell wrappers.

use std::fmt;

use anyhow::{Context, Result, bail};

use crate::vcs::Worktree;

/// Resolve a worktree from either an explicit name or an interactive pick.
///
/// `query` matching: exact name, then exact branch name, then case-insensitive
/// substring match on either. The picker's fuzzy filter handles the rest.
pub fn resolve<'a>(
    worktrees: &'a [Worktree],
    query: Option<&str>,
    non_interactive: bool,
    prompt: &str,
) -> Result<&'a Worktree> {
    if worktrees.is_empty() {
        bail!("no worktrees to pick from");
    }

    if let Some(q) = query {
        return resolve_by_query(worktrees, q);
    }

    if non_interactive {
        bail!("--non-interactive set and no worktree was named");
    }

    pick_interactive(worktrees, prompt)
}

fn resolve_by_query<'a>(worktrees: &'a [Worktree], query: &str) -> Result<&'a Worktree> {
    if let Some(w) = worktrees.iter().find(|w| w.name == query) {
        return Ok(w);
    }
    if let Some(w) = worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some(query))
    {
        return Ok(w);
    }

    let q_lower = query.to_lowercase();
    let matches: Vec<&Worktree> = worktrees
        .iter()
        .filter(|w| {
            w.name.to_lowercase().contains(&q_lower)
                || w.branch
                    .as_deref()
                    .is_some_and(|b| b.to_lowercase().contains(&q_lower))
        })
        .collect();

    match matches.len() {
        0 => bail!("no worktree matches `{query}`"),
        1 => Ok(matches[0]),
        n => {
            let names: Vec<&str> = matches.iter().map(|w| w.name.as_str()).collect();
            bail!(
                "`{query}` is ambiguous ({n} matches: {}); be more specific",
                names.join(", ")
            )
        }
    }
}

fn pick_interactive<'a>(worktrees: &'a [Worktree], prompt: &str) -> Result<&'a Worktree> {
    let items: Vec<Item<'_>> = worktrees.iter().map(Item).collect();
    let picked = inquire::Select::new(prompt, items)
        .prompt()
        .context("interactive pick cancelled")?;
    Ok(picked.0)
}

struct Item<'a>(&'a Worktree);

impl fmt::Display for Item<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let main = if self.0.is_main { " (main)" } else { "" };
        write!(f, "{}{}  {}", self.0.name, main, self.0.path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn wt(name: &str, branch: Option<&str>) -> Worktree {
        Worktree {
            name: name.into(),
            path: PathBuf::from(format!("/wt/{name}")),
            branch: branch.map(String::from),
            head: None,
            desc: None,
            dirty: false,
            conflict: false,
            is_main: false,
            is_bare: false,
            is_stale: false,
            is_locked: false,
        }
    }

    #[test]
    fn exact_name_wins() {
        let w = vec![wt("a", Some("a")), wt("ab", Some("ab"))];
        let r = resolve_by_query(&w, "a").unwrap();
        assert_eq!(r.name, "a");
    }

    #[test]
    fn substring_match_unique() {
        let w = vec![wt("foo", Some("feature/foo"))];
        let r = resolve_by_query(&w, "feature").unwrap();
        assert_eq!(r.name, "foo");
    }

    #[test]
    fn substring_match_case_insensitive() {
        let w = vec![wt("Foo", Some("Bar"))];
        let r = resolve_by_query(&w, "foo").unwrap();
        assert_eq!(r.name, "Foo");
    }

    #[test]
    fn ambiguous_substring_match_errors() {
        let w = vec![wt("alpha-one", None), wt("alpha-two", None)];
        let err = resolve_by_query(&w, "alpha").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn exact_name_wins_over_substring_collision() {
        // "alpha" exists exactly *and* would match "alphabet" by substring.
        // The exact match must win without triggering the ambiguity error.
        let w = vec![wt("alpha", None), wt("alphabet", None)];
        let r = resolve_by_query(&w, "alpha").unwrap();
        assert_eq!(r.name, "alpha");
    }

    #[test]
    fn no_match_errors() {
        let w = vec![wt("foo", None)];
        let err = resolve_by_query(&w, "bar").unwrap_err();
        assert!(err.to_string().contains("no worktree"));
    }

    #[test]
    fn non_interactive_without_query_errors() {
        let w = vec![wt("foo", None)];
        let err = resolve(&w, None, true, "pick").unwrap_err();
        assert!(err.to_string().contains("--non-interactive"));
    }
}
