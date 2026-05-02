# renri — ROADMAP

Design decisions and the staged work plan. Pinned at 2026-05.

---

## Positioning

**Local CLI** for managing parallel work across **git worktrees** and
**jj workspaces** — auto-detect the backend, expose one verb set,
configure with TOML+Tera via [teravars].

Out of scope (deliberately, from market survey):

- **Agent TUI.** claude-squad / ccmanager / workmux own the
  "fan-out N agents in tmux panes" niche; `renri` exposes `exec` and
  (later) an MCP server so those tools can sit on top.
- **Desktop GUI.** Crystal/Nimbalyst territory.
- **AI commit messages.** Worktrunk's moat.

The wide-open opportunities `renri` is targeting:

1. **First-class jj workspace support** — only `jj-navi` (14 stars)
   exists in this niche today.
2. **A `jj workspace prune` analog** — jj itself has no auto-detection
   of stale on-disk workspaces.
3. **Tera control flow + teravars include** for per-host / per-OS /
   team-base config sharing — no current tool does this.
4. **Honest cross-platform Windows support** — the existing tools are
   Unix-first with bolted-on shell shims.

---

## MVP scope (target: 0.1.0)

### Verbs

| verb | description | interactive fallback |
|---|---|---|
| `add <name?>` | Create worktree (git) or workspace (jj). | If `<name>` omitted, prompt for it. |
| `list` / `ls` | Show all worktrees. Includes jj-stale flag if applicable. | n/a |
| `remove <name?>` / `rm` | Remove worktree (git) / forget workspace (jj). | Fuzzy picker if `<name>` omitted. |
| `cd <name?>` | Print absolute path; intended for shell wrapper. | Fuzzy picker if `<name>` omitted. |
| `exec <name?> -- <argv>` | Run command in a worktree. | Fuzzy picker if `<name>` omitted. |
| `prune` | GC: removed dirs, jj-stale workspaces, broken `git worktree`. | n/a |
| `config` | Inspect / edit config. | n/a |

**Interactive UX**: any required argument that's missing falls back to
an `inquire`-based fuzzy picker, mirroring the rvpm pattern. Disabled
by `--non-interactive` (global flag, also implied when stdin is not a
tty — TBC).

### Backend dispatch

- Auto-detect by walking up from cwd: presence of `.jj/` ⇒ jj,
  presence of `.git/` ⇒ git. Colocated repo (`.jj/` and `.git/`
  both present) ⇒ default to jj, with `--vcs git` to override.
- Global `--vcs git|jj` flag forces a backend.

### Naming convention

Default worktree path template:

```
{{ wt_root }}/{{ vcs.owner }}/{{ vcs.repo }}/{{ vcs.branch | replace(from='/', to='-') }}
```

- `vcs.owner` — parsed from origin remote (`github.com:yukimemi/foo`
  → `yukimemi`). Fallback when no parseable origin: current user
  (`whoami::username()`).
- `vcs.repo` — repo name from origin, fallback to the directory name.
- `vcs.branch` — git branch / jj bookmark.
- `wt_root` — `~/wt` on Unix, `%USERPROFILE%/wt` on Windows.

All overridable via `[layout]` in `renri.toml`. Per-host / per-OS
overrides drop out for free thanks to teravars (`system.host`,
`system.os`, `is_windows()` etc).

### Hooks

Hybrid model: typed hooks for the common cases, `command` for the
escape hatch.

```toml
[[hooks.post_create]]
type = "copy"
files = [".env.example -> .env", "scripts/local.sh"]

[[hooks.post_create]]
type = "symlink"
src = "../node_modules"
dst = "node_modules"

[[hooks.post_create]]
type = "command"
shell = "auto"           # auto = pwsh on win, bash on unix; or explicit
run = "pnpm install"
```

Lifecycle for MVP: `post_create` and `pre_remove`. More phases
(`pre_switch`, `post_switch`, etc.) wait for v0.2 unless a need
emerges.

### Config

- File search order: `./renri.toml`, `$XDG_CONFIG_HOME/renri/config.toml`
  (Unix), `%APPDATA%/renri/config.toml` (Windows). Loaded via teravars
  `load_merged` so `include = [...]` works.
- Standard context exposed in templates: `system.{os, arch, user, host, cwd}`,
  `vcs.{owner, repo, branch, host}`, `env(...)`, `is_windows()`, ...

### MVP shell integration

A `renri shell-init <bash|zsh|fish|powershell>` subcommand emits a
shell function that wraps `cd "$(renri cd $@)"` so users can type
`renri cd foo` and actually change directory in the parent shell.

### MVP also ships

- `claude-skill.md` — a Markdown file users can drop into
  `~/.claude/skills/` so Claude Code knows the verbs and conventions.
  No MCP server in MVP.
- README, ROADMAP, LICENSE (MIT), CI matrix (linux/win/mac × default
  features), release workflow on tag.

---

## v0.2 candidates

- **MCP server** (`renri mcp serve`) for cross-AI agent integration
  (Phantom-style).
- **Deterministic resource allocation**: `port_offset(start, range)`,
  `hash` filter — so `[vars] dev_port = "{{ vcs.branch | hash | port_offset(start=3000, range=1000) }}"` Just Works. Filters live
  in teravars (so other CLIs benefit), wired up here as a default.
- **More hook phases**: `pre_switch`, `post_switch`, `pre_merge`,
  `post_merge`.
- **Pre-built binaries via GitHub Releases** alongside cargo publish.
- **PR-driven worktree creation** (`renri add --pr 123`).
- **Per-branch state vars** (`renri var set <key> <value>`) à la
  Worktrunk.

## v0.3 / not in scope (yet)

- Sandbox / container isolation per worktree.
- Multi-agent fan-out (claude-squad territory).
- Web UI / TUI dashboard.

---

## Architecture sketch

```
src/
  main.rs            # clap CLI, top-level dispatch, --non-interactive
  config.rs          # load renri.toml via teravars::load_merged
  vcs/
    mod.rs           # trait Backend; auto-detect dispatch
    git.rs           # git worktree wrapper
    jj.rs            # jj workspace wrapper
    detect.rs        # walk-up search for .git / .jj, colocate handling
  layout.rs          # render the worktree_path template
  hooks/
    mod.rs           # hook executor (sequence, error policy)
    copy.rs          # cross-platform file copy with `from -> to` syntax
    symlink.rs       # ln -s + Windows junction fallback
    command.rs       # spawn shell, capture stdout
  picker.rs          # inquire-based interactive fallback
  shell_init.rs      # emit shell function snippets
```

`teravars` is the entire config layer; `renri` provides:

1. Backend detection & shelling out to git/jj.
2. Hook execution.
3. Interactive picker.
4. Path rendering.

---

## Open questions

- **Branch ↔ worktree mapping for jj**: jj uses bookmarks (movable),
  git worktrees lock a branch. The default naming template uses
  `vcs.branch` — for jj this resolves to "the bookmark currently at
  the workspace's working-copy commit." Need to verify this is what
  users expect; `renri add foo` may need to *create* a bookmark with
  that name on the new workspace.
- **Stale tty / pipe handling for interactive fallback**: should
  `--non-interactive` be auto-set when `!stdin.is_tty()`? Probably yes;
  add a `--interactive` opposite flag for "force picker even when
  piped" if needed.
- **`renri config` UX**: just print the resolved config, or include
  edit / set / get verbs? MVP probably just `renri config show`.
