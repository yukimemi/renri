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
## Shared conventions

This file is the agent-agnostic source of truth (per the
[agents.md](https://agents.md) convention). The matching
`CLAUDE.md` and `GEMINI.md` files are thin shims that point back
here so each tool's auto-load behaviour still finds something.
**Edit AGENTS.md, not the shims.**

### Git workflow

- **No direct push to `main`.** Open a PR.
  - Exception: trivial typo / whitespace / docs wording fixes.
- Branch names: `feat/...`, `fix/...`, `chore/...`.
- **PR titles + bodies in English. Commit messages in English.**
- **Releases are PR-driven, tagging is automatic.** Bump
  `[workspace.package].version` (workspace) or `[package].version`
  (single crate) in a `chore/release-vX.Y.Z` PR. On merge to `main`,
  `.github/workflows/auto-tag.yml` (kata-managed) detects the bump,
  pushes the `vX.Y.Z` tag, and that tag fires `release.yml` for
  binary builds + crates.io publish. **Do not run `git tag` by
  hand** — the bot tag will collide and the manual push fails.

### PR review cycle

- Every PR runs reviews from **Gemini Code Assist** and
  **CodeRabbit**. Wait for both bots to post, address their
  comments (push fixes to the PR branch), and merge only after
  feedback is resolved.
- **After opening a PR, immediately enter the review-monitoring
  loop — do not ask the user whether to start it.** Drive the
  cadence with `/loop` — fixed-interval mode (e.g.
  `/loop 60s …`) schedules ticks via `CronCreate`; dynamic mode
  (no interval, `/loop …`) self-paces via `ScheduleWakeup`. The
  agent actively pulls fresh state each tick with
  `gh pr view <N> --json state,reviews,comments,statusCheckRollup`
  and `gh api repos/<owner>/<repo>/pulls/<N>/comments` (the
  latter covers inline review comments, which `gh pr view`
  does not surface) and reacts to new bot feedback. Passive
  watchers (background `gh` polls, file watchers, hooks) cannot
  trigger active follow-up, so they are not a substitute —
  without an active wake-up the agent never re-reads the PR.
- **Default polling interval: 60s.** Gemini Code Assist /
  CodeRabbit historically reply within ~1–3 minutes of a push or
  thread reply, so a 60s tick catches them on the next wake-up
  without burning cache: 60s sits well inside the 5-minute
  prompt-cache TTL, so the conversation context stays cached
  across ticks. Do **not** stretch the interval to 300s — that
  is the worst-of-both window (you pay the cache miss without
  amortizing it). If the PR is idle but a bot re-review is still
  expected (e.g. a CodeRabbit rate-limit refill window), step
  **up** to 1200–1800s instead.
- **Stop the loop entirely when only owner approval is missing.**
  Once review bots are quiet (or quiet-by-exception — version-bump
  skip, Renovate/Dependabot skip), CI is green, and there is no
  other expected follow-up, the *only* remaining action is human
  approval. GitHub already notifies the owner; the agent
  re-entering on every cron tick to find the same "still waiting
  on owner" state burns cache and adds no value. Stop scheduling
  further wake-ups (`CronDelete` in fixed-interval mode; simply
  omit the next `ScheduleWakeup` in dynamic mode) and report the
  wait state to the user. The owner restarts the loop after their
  next push if a fresh bot pass is wanted, or merges directly.
  (A CodeRabbit rate-limit window doesn't qualify on its own — a
  re-review is still expected once the quota refills, so step up
  to 1200–1800s instead and let it ride. Stopping is only correct
  when the owner has explicitly chosen to skip the bot pass per
  the rate-limit exception below.)
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
- **Version-bump-only PRs** (a single `chore/release-vX.Y.Z`
  branch whose entire diff is `[workspace.package].version` /
  `[package].version` + the matching inter-crate refs +
  `Cargo.lock`) **also skip the bot-review gate.** There is
  nothing for the bots to find in a version bump, and the
  release pipeline downstream of merge (auto-tag → release.yml)
  is time-sensitive. CI green + owner approval is enough.
- **Treat CodeRabbit rate-limit notices as "quiet" for the
  merge gate.** If CodeRabbit only posts a "Review limit
  reached" quota-exhaustion message (no findings, no inline
  comments), it has produced no review content — there is
  nothing to address. Re-trigger with `@coderabbitai review`
  once the quota refills if you want a real pass; for small or
  time-sensitive PRs, merge on owner approval without waiting.

### Worktree workflow

Use [`renri`](https://github.com/yukimemi/renri) for any
commit-bound change. From the main checkout:

```sh
renri add <branch-name> --from main@origin            # create a worktree (jj-first), off latest upstream main
renri --vcs git add <branch-name> --from origin/main  # force a git worktree, off latest upstream main
renri remove <branch-name> -y --non-interactive  # cleanup after merge (agent-safe; see note)
renri prune                        # GC stale worktrees
```

Read-only inspection can stay on the main checkout.

**Always pass `--from <upstream main>`** (`main@origin` for jj,
`origin/main` for git). Without it, `renri add` forks off the *cwd
worktree's current HEAD* — in a long-lived main checkout that often
lags upstream, so the PR later shows up CONFLICTING against a `main`
that had already moved (e.g. a refactor merged upstream before the
branch was cut), forcing a manual re-port of the whole change.
`renri add` does fetch first, but fetching only updates `main@origin`
— it never moves the checkout's HEAD, so an explicit `--from` is what
guarantees a fresh base.

**Agents / non-interactive shells:** `renri remove` prints a details
panel and waits for a confirmation prompt — without `-y` it **hangs**,
and `--non-interactive` *alone* errors asking for `-y`. Always pass
`-y`, and add `--non-interactive` so a mistyped/omitted name fails
instead of opening a fuzzy picker (the same picker-fallback applies to
`remove` / `cd` / `exec` with no name). Use `-f`/`--force` to remove a
worktree that still has uncommitted changes or conflicts. To sweep
every merged-PR worktree in one shot: `renri remove --merged -y`.

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

This repo follows the shared Rust toolchain conventions. The
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
fix to `yukimemi/pj-rust` so every Rust project using these templates picks
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

### Releasing: version bump PR + auto-tag

Releases are triggered from `main` by a Cargo.toml version
change. `.github/workflows/auto-tag.yml` is kata-managed (source:
`yukimemi/pj-rust/.github/workflows/auto-tag.yml.tera`). It
watches `main` and, whenever a commit lands that changes the
top-level `version = "..."` in `Cargo.toml`, it pushes a matching
`vX.Y.Z` tag — no manual `git tag` step is needed. The tag push
then fires `release.yml`; see `kata:agents:rust-lib:*` or
`kata:agents:rust-cli:*` for what release.yml does in each
crate shape.

Cut a release via a small PR — never `git push` the bump
straight to `main`, even though the base block lists version
bumps as an exception to "no direct push". `auto-tag.yml` only
fires on `main`-branch pushes, so the bump must land via a merge
either way; using a PR also gives CI a chance to gate the
release. Enable automerge so CI green = release start:

```sh
git switch -c chore/release-vX.Y.Z
# Edit `package.version` in Cargo.toml, then:
cargo build                     # let Cargo.lock follow
git commit -am "chore: release vX.Y.Z"
git push -u origin chore/release-vX.Y.Z
gh pr create --fill
gh pr merge --auto --squash --delete-branch
```

Once CI is green the PR auto-merges. `auto-tag.yml` then pushes
`vX.Y.Z`, which fires `release.yml`.

**Repo settings to set once:** enable
`delete_branch_on_merge=true` (Settings → General →
"Automatically delete head branches"). The `--delete-branch`
flag on `gh pr merge --auto` is effectively a no-op — gh
returns as soon as automerge is enabled, so the deletion has to
happen server-side, which requires the repo setting.

**Why `KATA_APPLY_TOKEN`:** GitHub refuses to fire downstream
workflows from tags pushed by the default `GITHUB_TOKEN`, so
`auto-tag.yml` pushes with `KATA_APPLY_TOKEN` (the same PAT
`kata-apply.yml` already uses). Each consumer repo needs a
`KATA_APPLY_TOKEN` secret set; if a version-bump merge silently
doesn't fire `release.yml`, the missing PAT is the first thing
to check.
<!-- kata:agents:rust:end -->
<!-- kata:agents:rust-cli:begin -->
### Rust CLI release flow

This is a Rust CLI crate, so the release pipeline is publish-aware.
`yukimemi/pj-rust-cli` ships a tag-driven release workflow in
`.github/workflows/release.yml` (rendered from
`release.yml.template` for the same don't-auto-execute reason
ci.yml uses).

Releases are triggered by a Cargo.toml version bump landing on
`main`. The bump flow itself (PR with automerge → `auto-tag.yml`
pushes `vX.Y.Z` → `release.yml` runs) is documented in
`kata:agents:rust:*` under "Releasing: version bump PR +
auto-tag" — that block also covers the `KATA_APPLY_TOKEN` and
`delete_branch_on_merge` setup. What `release.yml` then does for
a **CLI** crate:

1. Cross-compiles binaries for x86_64 Linux / Windows / macOS,
   plus aarch64 macOS (Apple Silicon) — full triples
   `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`,
   `x86_64-apple-darwin`, `aarch64-apple-darwin`.
2. Uploads them as a GitHub Release with auto-generated notes.
3. `cargo publish --locked` to crates.io using the
   `CARGO_REGISTRY_TOKEN` repo secret.

Set the `CARGO_REGISTRY_TOKEN` secret once per repo (`gh secret
set CARGO_REGISTRY_TOKEN`) before the first release. If the
crate is internal-only and shouldn't go to crates.io, either drop
the `publish` job locally (release.yml is `when = "once"` so the
edit survives subsequent applies) or set `package.publish = false`
in `Cargo.toml`.

The binary name is derived from the GitHub repo name at runtime
(`${{ github.event.repository.name }}`), so the workflow is
identical across CLIs using these templates unless your `[[bin]] name` in
`Cargo.toml` deliberately differs from the repo name — in that
case override `BIN_NAME` in the workflow's `env:` block.

### Release smoke target (`examples/smoke.rs`)

After `cargo build --release`, `release.yml` runs
`cargo run --release --target <T> --example smoke` on every build
matrix entry. `cargo test` runs only library code, so the produced
binary's startup path goes unverified — that's how shoka v0.10.0
shipped a rustls `CryptoProvider` panic to crates.io even though
all 13 CI checks were green.

The template's default `examples/smoke.rs` body is intentionally
no-op so kata can drop it into every consumer crate without
breaking releases. **Override it per crate** with the smallest
operation that exercises the regression-prone surface:

- HTTPS-using CLIs: build the API client (octocrab, reqwest, etc.)
  and issue a tiny no-auth GET — that forces the rustls handshake
  to run inside the same binary the release publishes.
- File-handling CLIs: write+read a temp file via the real I/O
  helpers (catches missing crate features, permission regressions).
- Pure library crates: leave as no-op.

A failing smoke blocks the release before publishing to GitHub
Releases / crates.io.
<!-- kata:agents:rust-cli:end -->
