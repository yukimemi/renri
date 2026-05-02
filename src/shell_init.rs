//! Emit a shell snippet that wraps `renri cd` so the parent shell actually
//! changes directory (since a child process can't `cd` for its parent).

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

const POSIX: &str = r#"# renri shell wrapper — paste into ~/.bashrc / ~/.zshrc
# Usage: `renri cd foo` actually cds the current shell.
renri() {
    if [ "$1" = "cd" ]; then
        local target
        target=$(command renri cd "${@:2}") || return $?
        [ -n "$target" ] && cd "$target"
    else
        command renri "$@"
    fi
}
"#;

const FISH: &str = r#"# renri shell wrapper — paste into ~/.config/fish/config.fish
function renri
    if test (count $argv) -ge 1; and test "$argv[1]" = "cd"
        set -l target (command renri cd $argv[2..-1])
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
        $target = & renri.exe cd @rest
        if ($LASTEXITCODE -eq 0 -and $target) {
            Set-Location -LiteralPath $target
        }
    } else {
        & renri.exe @args
    }
}
"#;
