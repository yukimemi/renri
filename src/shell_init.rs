//! Emit (and optionally install) a shell snippet that wraps `renri cd` so
//! the parent shell actually changes directory.
//!
//! The wrapper sets `RENRI_SHELL_WRAPPER=1` before invoking the binary so
//! the binary knows to print the worktree path (and let the wrapper `cd`)
//! instead of falling back to spawning a subshell.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

pub fn snippet(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash | Shell::Zsh => POSIX,
        Shell::Fish => FISH,
        Shell::Powershell => POWERSHELL,
    }
}

/// Append the wrapper snippet to the shell's startup file. Idempotent: if
/// the snippet is already present (matched by the marker comment), no-op.
pub fn install(shell: Shell) -> Result<PathBuf> {
    let target = rc_path(shell)?;
    let snippet_text = snippet(shell);

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // Treat `NotFound` as "create a fresh file"; bubble up everything else
    // (permission denied, IO error) so we don't overwrite an existing rcfile
    // we couldn't read.
    let existing = match std::fs::read_to_string(&target) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(anyhow::Error::from(e).context(format!(
                "reading {} for idempotency check",
                target.display()
            )));
        }
    };
    if existing.contains(MARKER) {
        return Ok(target);
    }

    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push('\n');
    new_content.push_str(snippet_text);

    std::fs::write(&target, new_content)
        .with_context(|| format!("writing {}", target.display()))?;
    Ok(target)
}

fn rc_path(shell: Shell) -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine $HOME")?;
    Ok(match shell {
        Shell::Bash => home.join(".bashrc"),
        Shell::Zsh => home.join(".zshrc"),
        Shell::Fish => home.join(".config/fish/config.fish"),
        Shell::Powershell => {
            if !cfg!(windows) {
                bail!(
                    "PowerShell auto-install is only supported on Windows; copy the snippet manually"
                );
            }
            let docs = dirs::document_dir().context("could not locate Documents")?;
            // Two profile dirs coexist on Windows: PowerShell 7+ ("PowerShell")
            // and Windows PowerShell 5.1 ("WindowsPowerShell"). Prefer the
            // edition whose profile dir already exists; default to PS7 (newer)
            // when neither is present.
            let ps7 = docs
                .join("PowerShell")
                .join("Microsoft.PowerShell_profile.ps1");
            let ps5 = docs
                .join("WindowsPowerShell")
                .join("Microsoft.PowerShell_profile.ps1");
            if ps7.parent().is_some_and(|p| p.exists()) {
                ps7
            } else if ps5.parent().is_some_and(|p| p.exists()) {
                ps5
            } else {
                ps7
            }
        }
    })
}

/// Marker substring used by the install command to detect "already
/// installed". Bumping it forces a re-install on the next run.
const MARKER: &str = "renri shell wrapper";

const POSIX: &str = r#"# renri shell wrapper — paste into ~/.bashrc / ~/.zshrc
# Usage: `renri cd foo` actually cds the current shell.
renri() {
    if [ "$1" = "cd" ]; then
        local target
        target=$(RENRI_SHELL_WRAPPER=1 command renri cd "${@:2}") || return $?
        [ -n "$target" ] && cd "$target"
    else
        command renri "$@"
    fi
}
"#;

const FISH: &str = r#"# renri shell wrapper — paste into ~/.config/fish/config.fish
function renri
    if test (count $argv) -ge 1; and test "$argv[1]" = "cd"
        set -l target (env RENRI_SHELL_WRAPPER=1 command renri cd $argv[2..-1])
        and test -n "$target"
        and cd $target
    else
        command renri $argv
    end
end
"#;

const POWERSHELL: &str = r#"# renri shell wrapper — paste into $PROFILE
function renri {
    if ($args.Count -ge 1 -and $args[0] -eq 'cd') {
        $rest = if ($args.Count -gt 1) { $args[1..($args.Count - 1)] } else { @() }
        $env:RENRI_SHELL_WRAPPER = '1'
        try {
            $target = & renri.exe cd @rest
            if ($LASTEXITCODE -eq 0 -and $target) {
                Set-Location -LiteralPath $target
            }
        } finally {
            Remove-Item Env:RENRI_SHELL_WRAPPER -ErrorAction SilentlyContinue
        }
    } else {
        & renri.exe @args
    }
}
"#;
