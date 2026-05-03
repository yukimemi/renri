# CLAUDE.md

Guidance for Claude Code when working in this repo.

## What renri is

A unified CLI for managing parallel work across **git worktrees** and
**jujutsu (jj) workspaces**, configured by TOML files driven through
the [teravars] library so the same config can `include = [...]`
others, render `{% if system.os == "windows" %}` per-OS sections, and
expose `system.*` / `vcs.*` to the user's templates.

Name comes from 連理 (renri), the classical Chinese / Japanese image
of two trees whose branches grow into each other. Crate name `renri`,
binary `renri`, repo `yukimemi/renri`.

## Source layout

```
src/
  main.rs            — entry point: clap derive, top-level dispatch
  lib.rs             — module list (binary uses `use renri::...`)
  vcs/
    mod.rs           — Backend trait, Worktree row struct, VcsChoice
                        + select_kind() (Auto / Git / Jj override policy)
                        + open_backend() factory
    detect.rs        — walk-up search for .git / .jj; returns Repo {root,kind}
    git.rs           — GitBackend: `git worktree {list,add,remove,prune}` wrapper
    jj.rs            — JjBackend: `jj workspace {add,list,forget,root}` wrapper
                        + the missing `jj workspace prune` analog
  config.rs          — Config struct (layout + hooks) loaded via
                        teravars::load_merged from <repo>/renri.toml +
                        <config_dir>/renri/config.toml
  layout.rs          — origin-URL parser (SCP / https / ssh) + Tera
                        path renderer with vcs.* in scope
  hooks/
    mod.rs           — typed hook executor (HookRun context, run_all)
    copy.rs          — cross-platform file/dir copy with `src -> dst`
    symlink.rs       — symlink (Unix) / symlink + junction fallback (Windows)
    command.rs       — pwsh / bash / sh / zsh / cmd dispatch
  picker.rs          — inquire-based fuzzy fallback for missing args
  shell_init.rs      — bash / zsh / fish / powershell wrapper snippets
```

## Key design decisions (don't rediscover)

These were settled during the initial design pass. Flag with the user
before reverting any of them.

- **One verb set, two backends.** Auto-detect from `.git/` / `.jj/`
  presence; `--vcs git|jj` override. Colocated repos default to **jj**
  per ROADMAP — the jj working-copy model is the source of truth when
  both are present. `select_kind()` in `vcs/mod.rs` is the single
  policy point.
- **Naming convention default** is `~/wt/<owner>/<repo>/<branch>`,
  parsed from origin remote with fallback to current user's name when
  no parseable origin. All overridable via `[layout]` in
  `renri.toml`. Layout templates can use `system.os`, `system.host`,
  `vcs.*` so per-host / per-OS dispatch is one `{% if %}` away.
- **Hooks are typed** — `type = "copy" | "symlink" | "command"` in
  TOML, not raw shell strings. `command` is the escape hatch.
  `copy` / `symlink` are first-class because they're the most common
  use case and `command` would force users to write portability
  shims for `cp` / `ln`.
- **Interactive fallback for any missing required argument.**
  rvpm-style: `renri cd` opens an `inquire::Select` picker;
  `renri cd <name>` skips it. `--non-interactive` (or any flag we
  decide implies it) makes the missing-arg case an error. Picker
  TUI lives on stderr so stdout stays clean for shell wrappers.
- **`renri cd` prints the path on stdout**, designed for shell
  function wrappers like `cd "$(renri cd foo)"`. The `shell-init`
  verb emits a wrapper that does this automatically. The function
  uses `command renri` (POSIX) or `renri.exe` (PowerShell) to
  bypass the same-named function.
- **Deferred-template trick for vcs.** teravars's `load_merged`
  Tera-renders every string in the config at load time, but
  `{{ vcs.repo }}` inside layout templates needs to be deferred
  until the actual branch is known. We pre-populate the load-time
  context with `vcs = { owner: "{{ vcs.owner }}", ... }` so those
  references round-trip through Tera unchanged. `system.*` / `env(...)`
  / `{% if %}` blocks DO render at load time as expected.
- **Don't ship symlink-without-Developer-Mode for files on Windows.**
  Surface a clear error pointing the user at the right setting.
  Directories transparently fall back to `mklink /J` (junction) which
  doesn't need any privilege. This is documented in
  `src/hooks/symlink.rs`.
- **`[teravars] include = [...]`** is the namespaced fallback if a
  user's app config legitimately uses `include` as a top-level key.
  Both forms in the same file is `Error::IncludeConflict`.
- **jj `prune` is implemented manually** because there's no upstream
  `jj workspace prune`. We list workspaces and forget the ones whose
  root path is gone. This is one of the main reasons renri exists
  for jj users — flag with the user before changing how it behaves.
- **AI integration goes through APM.** The skill at
  `.apm/skills/renri/SKILL.md` is the single source of truth;
  Microsoft's [APM](https://github.com/microsoft/apm) compiles it
  into the right format for Copilot / Claude Code / Cursor /
  OpenCode / Codex / Gemini on `apm install yukimemi/renri`. We
  intentionally do **not** ship a parallel Claude-only `skill.md` —
  duplicate content drifts. MCP server is still a v0.2 item.

## Development

**Practice TDD.** Red-green-refactor.

```bash
cargo make setup                    # one-time on clone: hook + apm install
cargo test                          # unit + (eventual) integration
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo make check                    # all of the above (pre-push gate)
```

`cargo make setup` is `hook-install` + `apm-install` — runs once
per clone. Individual tasks:

- `cargo make hook-install` — wires `.git/hooks/pre-push` to
  `cargo make check`.
- `cargo make apm-install` — runs `apm install`, compiling renri's
  own skill from `.apm/skills/renri/SKILL.md` into
  `.claude/skills/renri/SKILL.md` + `.gemini/skills/renri/SKILL.md` +
  `.github/skills/renri/SKILL.md`. **Requires the
  [APM](https://github.com/microsoft/apm) CLI on `PATH`** —
  `scoop install apm` (Windows), `brew install microsoft/apm/apm`
  (macOS), `pip install apm-cli`, or
  `curl -sSL https://aka.ms/apm-unix | sh`.
  renri **dogfoods APM** — the source-of-truth skill lives in
  `.apm/`, the compiled outputs are committed for visibility, and
  `apm.lock.yaml` pins the resolution. When the skill content
  changes, re-run `cargo make apm-install` and commit the updated
  files.

renri is itself **a publishable APM package** (`apm install
yukimemi/renri` resolves to a tag matching `apm.yml`'s `version`).
That's why `apm.yml` carries a real, bumping version — when cutting
a release, bump `Cargo.toml`, `Cargo.lock`, and `apm.yml` together,
then tag.

## Working in this repo with AI agents

- **Read-only inspection** (browsing files, answering questions,
  running read-only commands): no worktree needed; work in the
  existing checkout.
- **Any commit-bound change** — new feature, bug fix, refactor,
  reviewer-feedback fix on an open PR: if you are on the **main
  checkout**, start with `renri add <branch-name>` and move into
  the worktree before committing (`cd "$(renri cd <branch-name>)"`,
  or use the shell wrapper from `renri shell-init` so plain
  `renri cd <name>` cds for you). If you are **already in a
  worktree** (e.g. iterating on an existing PR), keep working
  there. Do **not** edit on the main checkout for non-trivial
  changes.
- **Trivial wording / typo fixes** are the only soft exception, and
  even then `renri add` is cheap enough that defaulting to it is
  fine.

### Backend choice — jj-first

This repo is colocated git+jj. `renri add` defaults to **jj**
(creates a non-colocated jj workspace where `jj` commands work and
`git` does not — see jj-vcs/jj#8052 for why secondary colocation
isn't possible yet). Stick to the default unless there is a
specific reason to use git tooling.

```sh
# In a freshly created worktree (default jj backend):
jj st                                     # status
jj describe -m "feat: ..."                # set @-commit description
jj git push --bookmark <branch> --allow-new   # first push of a new branch
jj git push --bookmark <branch>           # subsequent pushes
```

`renri --vcs git add <branch>` is the override and exists for
genuine git-CLI-only needs (git submodule, native git2 tooling,
git-only hooks). Do **not** reach for it out of git-CLI familiarity
— prefer learning the equivalent jj commands.

### Cleanup after merge

After the PR merges and you've pulled the change into main:

- `renri remove <branch>` — removes a single worktree. Calls
  `git worktree remove` or `jj workspace forget` as appropriate,
  then deletes the directory. Refuses to remove the main worktree.
- `renri prune` — best-effort GC across the repo. Git: removes
  worktree metadata for already-deleted directories. jj: forgets
  workspaces whose root path is gone (the missing
  `jj workspace prune` analog).

Run `renri prune` periodically — especially after manually
`rm -rf`-ing worktree dirs without going through `renri remove`.

### Hooks in worktrees

The pre-push hook installed by `cargo make hook-install` lives in
the **main repo's** `.git/hooks/pre-push`.

- **git worktrees** share that hook directory, so plain `git push`
  from a worktree triggers `cargo make check` automatically.
- **jj workspaces** route their pushes through `jj git push`, which
  uses libgit2 directly and **does not fire git hooks**. From a jj
  workspace, run `cargo make check` manually before
  `jj git push --bookmark <branch>` — there is no automatic gate.

The renri skill (compiled into `.claude/skills/` etc. by
`cargo make apm-install`) tells the agent the **how**; this section
is the **when** + the **policy** for this repo.

`cargo make check` mirrors CI exactly. The pre-push hook runs it,
so failed checks block push.

## Resilience principle

A single failure should not stop the whole tool, *unless* it would
leave the repo in an inconsistent state. Specifically:

- VCS detection failure → clear error pointing the user at `cd`-ing
  into a real repo.
- Layout rendering failure → bail; we can't pick a target path safely.
- Backend `add` / `remove` failure → propagate; the underlying VCS
  state is the source of truth.
- Hook failure → bail; partially-applied hooks are worse than none,
  and the worktree directory will likely already exist.
- `prune` per-entry failure → log and continue; pruning is best-effort.

## Git workflow

- **No direct push to `main`.** Open a PR.
  - Exception: trivial typo / whitespace / docs wording fixes.
  - Exception: standalone version bumps (`Cargo.toml` + `Cargo.lock`
    refresh + `git tag vX.Y.Z`).
- Branch names describe the change (`feat/...`, `fix/...`).
- **PR titles + bodies in English. Commit messages in English.**
- Tag-based releases: `git tag vX.Y.Z && git push origin vX.Y.Z`. The
  `release.yml` workflow verifies tag-vs-Cargo.toml consistency and
  publishes to crates.io.

### PR review cycle

- Every PR triggers **Gemini Code Assist** and **CodeRabbit** reviews.
  Wait for both, address comments (push fixes to the PR branch), and
  merge only after feedback resolves.
- **Reply to the reviewer after pushing a fix.** Post a reply in the
  comment thread with `@gemini-code-assist` / `@coderabbitai` so the
  bot knows the feedback was acted on. Silent fixes lose the audit
  trail and trigger blind re-review.
- **Settle rule**: a thread settles when the latest bot reply is
  ack-only (a re-review summary with no new findings). New actionable
  comments un-settle it.
- **Stop conditions**:
  1. All open threads settled.
  2. No bot reply for 30 min after the last actionable comment.
- **Merge gate**: review bots stopped posting actionable comments
  AND @yukimemi has approved.
- **Bot-authored PRs (Renovate / Dependabot)**: review bots skip them
  by default, so the wait gate doesn't apply. Merge if CI is green
  and owner approves.

## Useful invocations

```sh
# List worktrees in the current repo
cargo run --quiet -- list

# Show the resolved config + computed path for the current branch
cargo run --quiet -- config show

# Generate the bash wrapper
cargo run --quiet -- shell-init bash

# Override the default templates per-call (debug)
cargo run --quiet -- --vcs git list
cargo run --quiet -- --non-interactive cd main
```

## Version + changelog

Version lives only in `Cargo.toml`. `cargo check` refreshes
`Cargo.lock` after a bump. Commit titles follow
`<type>: <summary> (vX.Y.Z)` (e.g. `feat: ... (v0.1.0)`) so the
release surface is traceable from `git log`.

[teravars]: https://github.com/yukimemi/teravars
