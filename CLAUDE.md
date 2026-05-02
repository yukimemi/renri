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
cargo test                          # unit + (eventual) integration
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo make check                    # all of the above (pre-push gate)
cargo make hook-install             # install pre-push hook (one-time)

apm install                         # compile renri's own skill into
                                    # .github/skills/ so AI agents in
                                    # this repo know about renri
```

`cargo make check` mirrors CI. The pre-push hook should be installed
on checkout so failed checks block push.

renri **dogfoods APM on itself.** The skill source of truth is
`.apm/skills/renri/SKILL.md`; running `apm install` from the repo
root compiles it into `.github/skills/renri/SKILL.md` (and that
location is committed so new contributors see the skill before
running APM). The lockfile `apm.lock.yaml` is also committed. When
the skill content changes, re-run `apm install` and commit both.

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
