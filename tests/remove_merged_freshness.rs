//! `remove --merged` decides by content, and content is a comparison
//! against a **local** ref. This is the one behaviour in renri that cannot
//! be pinned by a unit test: the bug it guards against lives entirely in
//! the I/O — the policy in `sweep.rs` was always right, and still reported
//! "nothing to remove" for a worktree that had just merged.
//!
//! So the fixture is a real repo: a bare `origin`, a clone, a renri-created
//! worktree whose commit is applied upstream *by someone else*, and a
//! `origin/main` in the clone that nobody fetched. That is the shape of
//! every "merge the PR, then sweep" session, and it is the shape the test
//! keeps working.
//!
//! git only, deliberately. The same staleness hits jj harder — there
//! `dirty` means "`@` has content", so a stale content verdict does not
//! merely mislabel the row, it keeps it — but a jj binary is not something
//! CI is guaranteed to have, and the fetch being fixed is shared by both
//! backends. `git` missing skips the test rather than failing it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A scenario: bare origin, working clone, and a renri worktree whose work
/// is upstream but whose `origin/main` has not been fetched since.
struct Fixture {
    _dir: tempfile::TempDir,
    work: PathBuf,
    worktree: PathBuf,
    home: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn have_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Run the built binary with the user's real environment held at arm's
/// length: a temp `HOME` / `APPDATA` so `<config_dir>/renri/config.toml`
/// cannot reach in, and the auto-update kill-switch so no test touches the
/// network.
fn renri(fx: &Fixture, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_renri"))
        .current_dir(&fx.work)
        .args(args)
        .env("RENRI_NO_AUTOUPDATE", "1")
        .env("HOME", &fx.home)
        .env("APPDATA", &fx.home)
        .env("XDG_CONFIG_HOME", &fx.home)
        .output()
        .expect("spawn renri");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn setup() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let root = root.as_path();
    let origin = root.join("origin.git");
    let work = root.join("work");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    git(root, &["init", "--bare", "-b", "main", "origin.git"]);
    git(root, &["init", "-b", "main", "work"]);
    git(&work, &["config", "user.email", "t@example.com"]);
    git(&work, &["config", "user.name", "tester"]);
    // The fixture writes LF and must read LF back. With the developer's own
    // `core.autocrlf = true` inherited from the global config, the checkout
    // gets CRLF and the next `add` records a normalisation diff on files
    // this test never touched — which lands in the commit and changes its
    // patch-id, so the upstream copy stops matching and the sweep is right
    // to leave the row alone. Cost an hour once; pinned here.
    git(&work, &["config", "core.autocrlf", "false"]);
    git(&work, &["config", "core.eol", "lf"]);

    // Worktrees land inside the fixture, not in `~/wt`. Forward slashes:
    // a Windows path in TOML is a string full of escape sequences.
    let wt_root = root.join("wt").to_string_lossy().replace('\\', "/");
    std::fs::write(
        work.join("renri.toml"),
        format!("[layout]\nworktree_root = '{wt_root}'\nworktree_path = '{{{{ vcs.branch }}}}'\n"),
    )
    .unwrap();
    std::fs::write(work.join("base.txt"), "base\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-qm", "base"]);
    // An absolute origin URL: a relative one resolves against the cwd,
    // which for a worktree is not the main checkout.
    let origin_url = origin.to_string_lossy().replace('\\', "/");
    git(&work, &["remote", "add", "origin", &origin_url]);
    git(&work, &["push", "-q", "-u", "origin", "main"]);

    let mut fx = Fixture {
        _dir: dir,
        work,
        worktree: PathBuf::new(),
        home,
    };

    let (ok, out) = renri(
        &fx,
        &[
            "--vcs",
            "git",
            "--non-interactive",
            "add",
            "feature",
            "--from",
            "origin/main",
        ],
    );
    assert!(ok, "renri add failed: {out}");
    fx.worktree = root.join("wt").join("feature");
    assert!(fx.worktree.is_dir(), "worktree not created: {out}");

    // Work committed in the worktree, and never pushed — the branch name is
    // not the signal here, the patch is.
    std::fs::write(fx.worktree.join("feature.txt"), "feature\n").unwrap();
    // Named, not `-A`: the commit has to be exactly the patch the upstream
    // copy will carry, or the patch-ids diverge and the test measures the
    // fixture instead of renri.
    git(&fx.worktree, &["add", "feature.txt"]);
    git(&fx.worktree, &["commit", "-qm", "feat: add feature.txt"]);

    // Somebody else lands the same diff on main and pushes it, the way a
    // squash-merge does. The clone's `origin/main` still points at `base`.
    let patch = git(&fx.worktree, &["format-patch", "-1", "--stdout"]);
    let merger = root.join("merger");
    git(root, &["clone", "-q", &origin_url, "merger"]);
    git(&merger, &["config", "user.email", "t@example.com"]);
    git(&merger, &["config", "user.name", "tester"]);
    git(&merger, &["config", "core.autocrlf", "false"]);
    git(&merger, &["config", "core.eol", "lf"]);
    let patch_file = root.join("upstream.patch");
    std::fs::write(&patch_file, patch).unwrap();
    git(
        &merger,
        &["am", "-q", patch_file.to_string_lossy().as_ref()],
    );
    git(&merger, &["push", "-q", "origin", "main"]);

    fx
}

/// The clone is behind on purpose: this is the state every sweep starts in.
fn origin_main_is_stale(fx: &Fixture) -> bool {
    let cached = git(&fx.work, &["rev-parse", "origin/main"]);
    let upstream = git(&fx.work, &["ls-remote", "origin", "refs/heads/main"]);
    !upstream.starts_with(cached.trim())
}

#[test]
fn merged_sweep_fetches_before_judging_content() {
    if !have_git() {
        eprintln!("skipping: no git binary");
        return;
    }
    let fx = setup();
    assert!(
        origin_main_is_stale(&fx),
        "fixture is not stale, so it proves nothing"
    );

    let (ok, out) = renri(&fx, &["--non-interactive", "remove", "--merged", "-y"]);
    assert!(ok, "sweep failed: {out}");
    assert!(
        !fx.worktree.exists(),
        "worktree survived a sweep that should have found its content \
         upstream:\n{out}\ncherry vs origin/HEAD (`-` = upstream):\n{}",
        git(
            &fx.work,
            &["cherry", "refs/remotes/origin/HEAD", "refs/heads/feature"]
        ),
    );
}

#[test]
fn no_fetch_judges_against_the_cached_ref() {
    if !have_git() {
        eprintln!("skipping: no git binary");
        return;
    }
    let fx = setup();

    let (ok, out) = renri(
        &fx,
        &[
            "--non-interactive",
            "remove",
            "--merged",
            "-y",
            "--no-fetch",
        ],
    );
    // Nothing to sweep is not an error, and the worktree has to survive:
    // against the cached ref its content genuinely is not upstream yet.
    assert!(ok, "sweep errored: {out}");
    assert!(
        fx.worktree.is_dir(),
        "--no-fetch swept a row whose content the cached ref does not have:\n{out}"
    );
}
