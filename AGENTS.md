# AGENTS.md

Guidance for AI agents (Claude / Codex / Gemini) working in this
repo. The yukimemi/* shared conventions live in the
`<!-- kata:agents:* -->` blocks below, sourced from
`yukimemi/pj-base` / `pj-rust` / `pj-rust-cli` via `kata apply` —
see those for git workflow, PR review cycle, build/lint/test
commands, release flow, and renri's own worktree usage patterns.

The sections above the marker blocks are renri-specific and
consumer-owned: edit them freely; `kata apply` won't touch them.

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

## renri-specific APM dogfood

Unlike most yukimemi/* repos that **consume** APM (apm.yml lists
`yukimemi/renri#main` as a dependency to pull the renri skill into
agent skill dirs), renri itself **publishes** that skill. So in
this repo:

- The single source of truth lives at `.apm/skills/renri/SKILL.md`.
- `cargo make apm-install` compiles it into `.claude/skills/` /
  `.gemini/skills/` / `.github/skills/` (the same step downstream
  consumers run).
- The compiled outputs are committed for visibility.
- `apm.lock.yaml` pins the resolution.
- `apm.yml` carries a **real, bumping version** (NOT the static
  `0.0.0` placeholder used by consumer repos), because
  `apm install yukimemi/renri@vX.Y.Z` resolves to the matching
  tag.

When the skill content changes, re-run `cargo make apm-install`
and commit the regenerated files. When cutting a release, bump
`Cargo.toml`, `Cargo.lock`, and `apm.yml` together, then tag.

## renri-specific tooling notes

The base / rust / rust-cli marker blocks below cover the
yukimemi/* common toolchain (cargo make, renri itself for
worktrees, jj-first workflow, release flow). Two repo-specific
elaborations that matter when working IN renri:

### jj-first colocation

This repo is colocated git+jj. `renri add` defaults to **jj**,
which creates a non-colocated jj workspace where `jj` commands
work and `git` does not — see
[jj-vcs/jj#8052](https://github.com/jj-vcs/jj/issues/8052) for
why secondary colocation isn't possible yet. Stick to the jj
default unless there's a specific reason to use git tooling.

### Hooks in jj workspaces don't fire

The pre-push hook installed by `cargo make hook-install` lives
in the main repo's `.git/hooks/pre-push`.

- **git worktrees** share that hook directory, so plain
  `git push` from a worktree triggers `cargo make check`
  automatically.
- **jj workspaces** push via `jj git push`, which uses libgit2
  directly and **does not fire git hooks**. From a jj workspace,
  run `cargo make check` manually before
  `jj git push --bookmark <branch-name>` — there's no automatic
  gate.

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

<!-- kata:agents:base:begin -->
## yukimemi/* shared conventions

This file is the agent-agnostic source of truth (per the
[agents.md](https://agents.md) convention). The matching
`CLAUDE.md` and `GEMINI.md` files are thin shims that point back
here so each tool's auto-load behaviour still finds something.
**Edit AGENTS.md, not the shims.**

### Git workflow

- **No direct push to `main`.** Open a PR.
  - Exception: trivial typo / whitespace / docs wording fixes.
  - Exception: standalone version bumps.
- Branch names: `feat/...`, `fix/...`, `chore/...`.
- **PR titles + bodies in English. Commit messages in English.**
- Tag-based releases: `git tag vX.Y.Z && git push origin vX.Y.Z`.

### PR review cycle

- Every PR runs reviews from **Gemini Code Assist** and
  **CodeRabbit**. Wait for both bots to post, address their
  comments (push fixes to the PR branch), and merge only after
  feedback is resolved.
- **Reply to reviewers after pushing a fix.** Reply on the
  corresponding review thread with an **@-mention**
  (`@gemini-code-assist` / `@coderabbitai`). Silent fixes are
  invisible to reviewers and cost the audit trail.
- A review thread is **settled** the moment the latest bot reply
  is ack-only ("Thank you" / "Understood" / a re-review summary
  with no new findings) or 30 minutes elapse with no actionable
  comment.
- **Merge gate**: review bots quiet AND owner explicit approval.
- Bot-authored PRs (Renovate / Dependabot) skip the bot-review
  gate; CI green + owner approval is enough.

### Worktree workflow

Use [`renri`](https://github.com/yukimemi/renri) for any
commit-bound change. From the main checkout:

```sh
renri add <branch-name>            # create a worktree (jj-first)
renri --vcs git add <branch-name>  # force a git worktree
renri remove <branch-name>         # cleanup after merge
renri prune                        # GC stale worktrees
```

Read-only inspection can stay on the main checkout.

### kata-managed sections

Several files in this repo are managed by `kata apply` from the
[`yukimemi/pj-presets`](https://github.com/yukimemi/pj-presets)
templates — the bytes between `<!-- kata:*:begin -->` and
`<!-- kata:*:end -->` markers, plus the overwrite-always files
listed in `.kata/applied.toml`. **Editing those bytes locally
won't survive the next `kata apply`** — push the change to the
upstream template repo (`yukimemi/pj-base` / `yukimemi/pj-rust` /
…) instead. The marker scopes are layered:

- `kata:agents:base:*` — language-agnostic conventions (this section).
- `kata:agents:rust:*` — added when `pj-rust` applies.
- `kata:agents:rust-cli:*` — added when `pj-rust-cli` applies.
<!-- kata:agents:base:end -->
<!-- kata:agents:rust:begin -->
### Rust workflow

This repo follows the yukimemi/* Rust toolchain conventions. The
language-agnostic conventions block above (`kata:agents:base:*`)
covers git workflow, PR review cycle, and worktree usage.

### Build / lint / test

```sh
cargo make check                    # fmt --check + clippy + test + lock-check (the pre-push gate)
cargo make setup                    # one-time hook install + apm install
cargo build                         # debug build
cargo build --release               # release build
cargo test                          # tests; add -- --nocapture for stdout
```

`cargo make check` is what `.github/workflows/ci.yml` runs and what
the local pre-push hook calls — anything that passes locally
should pass on CI and vice versa. Don't paper over a failing
clippy by sprinkling `#[allow(clippy::...)]`; fix the underlying
issue or push back on the lint with reasoning.

### Toolchain pin

The Rust toolchain is pinned via `rust-toolchain.toml` and the
project compiles with the `stable` channel. Don't introduce
nightly-only features without a real reason; if you do, document
the reason in the relevant module.

### Lint / format policy

`rustfmt.toml` and `clippy.toml` are kata-managed (sourced from
`yukimemi/pj-rust`). Edits to those files in this repo won't
survive the next `kata apply`; if a setting is wrong, push the
fix to `yukimemi/pj-rust` so every yukimemi/* Rust project picks
it up.

### CI workflow

`.github/workflows/ci.yml` is also kata-managed. The source lives
in `yukimemi/pj-rust/.github/workflows/ci.yml.template` (the
`.template` suffix keeps GitHub Actions from running the source
itself in pj-rust); each Rust project receives the rendered
`ci.yml` via `kata apply`. Action versions are bumped centrally
by Renovate at `yukimemi/pj-rust` and propagate down on the next
apply, so don't bump them locally — Renovate is configured
(via the kata-distributed `renovate.json`) to ignore
`.github/workflows/ci.yml` and `.github/workflows/release.yml`
in each PJ to avoid the bump→clobber loop.
<!-- kata:agents:rust:end -->
<!-- kata:agents:rust-cli:begin -->
### Rust CLI release flow

This is a Rust CLI crate, so the release pipeline is publish-aware.
`yukimemi/pj-rust-cli` ships a tag-driven release workflow in
`.github/workflows/release.yml` (rendered from
`release.yml.template` for the same don't-auto-execute reason
ci.yml uses).

```sh
# Bump `package.version` in Cargo.toml (run `cargo build` so
# Cargo.lock follows), then:
git commit -am "chore: bump version to X.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin main vX.Y.Z
```

The workflow then:
1. Cross-compiles binaries for x86_64 Linux / Windows / macOS,
   plus aarch64 macOS (Apple Silicon) — full triples
   `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`,
   `x86_64-apple-darwin`, `aarch64-apple-darwin`.
2. Uploads them as a GitHub Release with auto-generated notes.
3. `cargo publish --locked` to crates.io using the
   `CARGO_REGISTRY_TOKEN` repo secret.

Set the `CARGO_REGISTRY_TOKEN` secret once per repo (`gh secret
set CARGO_REGISTRY_TOKEN`) before the first tag push. If the
crate is internal-only and shouldn't go to crates.io, either drop
the `publish` job locally (release.yml is `when = "once"` so the
edit survives subsequent applies) or set `package.publish = false`
in `Cargo.toml`.

The binary name is derived from the GitHub repo name at runtime
(`${{ github.event.repository.name }}`), so the workflow is
identical across yukimemi/* CLIs unless your `[[bin]] name` in
`Cargo.toml` deliberately differs from the repo name — in that
case override `BIN_NAME` in the workflow's `env:` block.
<!-- kata:agents:rust-cli:end -->
