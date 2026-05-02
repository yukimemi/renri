use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Run an arbitrary shell snippet inside the new worktree directory.
///
/// `shell` selects the interpreter:
/// - `None` / `Some("auto")` — pwsh on Windows, bash on Unix.
/// - `Some("pwsh" | "powershell")` — `powershell -NoProfile -Command <run>`.
/// - `Some("bash" | "sh" | "zsh")` — `<shell> -c <run>`.
/// - `Some("cmd")` — `cmd /C <run>`.
pub fn run(run: &str, shell: Option<&str>, cwd: &Path) -> Result<()> {
    let shell = match shell.unwrap_or("auto") {
        "auto" if cfg!(windows) => "pwsh",
        "auto" => "bash",
        s => s,
    };

    let (program, args): (&str, Vec<&str>) = match shell {
        "pwsh" => ("pwsh", vec!["-NoProfile", "-Command", run]),
        "powershell" => ("powershell", vec!["-NoProfile", "-Command", run]),
        "bash" => ("bash", vec!["-c", run]),
        "sh" => ("sh", vec!["-c", run]),
        "zsh" => ("zsh", vec!["-c", run]),
        "cmd" => ("cmd", vec!["/C", run]),
        other => bail!("unknown shell: {other}"),
    };

    let status = Command::new(program)
        .args(&args)
        .current_dir(cwd)
        .status()
        .with_context(|| format!("failed to spawn `{program}`"))?;

    if !status.success() {
        bail!("`{program}` exited with {status}");
    }
    Ok(())
}
