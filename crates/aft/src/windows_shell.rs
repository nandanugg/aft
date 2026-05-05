//! Shared Windows shell selection for foreground and background bash commands.
//!
//! Mirrors OpenCode's resolver: prefer modern PowerShell (`pwsh.exe`), fall
//! back to Windows PowerShell (`powershell.exe`), then to `cmd.exe`.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowsShell {
    /// PowerShell 7+ (cross-platform). Supports `&&` pipeline operator.
    Pwsh,
    /// Windows PowerShell 5.1 (legacy, still default on most Windows desktops
    /// but **absent on Windows 11 IoT Enterprise LTSC SKUs** — issue #27).
    /// Does NOT support `&&` in pipelines (PS 7+ only feature).
    Powershell,
    /// `cmd.exe` — the universal fallback. Present on every Windows SKU.
    /// Supports `&&` and `||` natively. Lacks PowerShell's piping/cmdlets but
    /// handles bash-style chained shell invocations correctly.
    Cmd,
}

impl WindowsShell {
    /// Binary name to spawn. Caller relies on PATH lookup.
    pub(crate) fn binary(self) -> &'static str {
        match self {
            WindowsShell::Pwsh => "pwsh.exe",
            WindowsShell::Powershell => "powershell.exe",
            WindowsShell::Cmd => "cmd.exe",
        }
    }

    /// Argument vector to pass alongside the user's command string.
    /// PowerShell variants take `-Command <string>`; cmd takes `/D /C <string>`
    /// (`/D` disables AutoRun macros that could otherwise inject env-trust
    /// behavior into our isolated invocation).
    pub(crate) fn args<'a>(self, command: &'a str) -> Vec<&'a str> {
        match self {
            WindowsShell::Pwsh | WindowsShell::Powershell => vec![
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                command,
            ],
            WindowsShell::Cmd => vec!["/D", "/C", command],
        }
    }

    pub(crate) fn command(self, command: &str) -> Command {
        let mut cmd = Command::new(self.binary());
        cmd.args(self.args(command));
        cmd
    }

    /// Wrap a background command so shell termination writes an exit marker.
    /// The marker is written via temp-file + rename for PowerShell variants and
    /// via `move /Y` for cmd.exe, matching the Unix background wrapper contract.
    pub(crate) fn wrapper_script(self, command: &str, exit_path: &Path) -> String {
        match self {
            WindowsShell::Pwsh | WindowsShell::Powershell => {
                let exit_path = powershell_single_quote(&exit_path.display().to_string());
                let binary = powershell_single_quote(self.binary());
                let command = powershell_single_quote(command);
                format!(
                    concat!(
                        "$exitPath = {exit_path}; ",
                        "$tmpPath = \"$exitPath.tmp.$PID\"; ",
                        "$global:LASTEXITCODE = $null; ",
                        "& {binary} -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command {command}; ",
                        "$success = $?; ",
                        "$nativeCode = $global:LASTEXITCODE; ",
                        "if ($null -ne $nativeCode) {{ $code = [int]$nativeCode }} ",
                        "elseif ($success) {{ $code = 0 }} ",
                        "else {{ $code = 1 }}; ",
                        "[System.IO.File]::WriteAllText($tmpPath, [string]$code); ",
                        "Move-Item -LiteralPath $tmpPath -Destination $exitPath -Force; ",
                        "exit $code"
                    ),
                    exit_path = exit_path,
                    binary = binary,
                    command = command
                )
            }
            WindowsShell::Cmd => {
                let tmp_path = format!("{}.tmp", exit_path.display());
                format!(
                    "{command} & echo %ERRORLEVEL% > {tmp} & move /Y {tmp} {exit}",
                    command = command,
                    tmp = cmd_quote(&tmp_path),
                    exit = cmd_quote(&exit_path.display().to_string())
                )
            }
        }
    }
}

/// Resolve which Windows shell to use for `bash` invocations.
///
/// Cached after the first resolve to avoid repeated PATH probes — the user's
/// installed shells don't change mid-session, so a static cache is safe and
/// keeps bash dispatch fast.
pub(crate) fn resolve_windows_shell() -> WindowsShell {
    static RESOLVED: OnceLock<WindowsShell> = OnceLock::new();
    *RESOLVED.get_or_init(|| resolve_windows_shell_with(|binary| which::which(binary).is_ok()))
}

pub(crate) fn resolve_windows_shell_with<F>(exists: F) -> WindowsShell
where
    F: Fn(&str) -> bool,
{
    if exists("pwsh.exe") {
        log::info!(
            "[aft] bash shell resolved to pwsh.exe (PowerShell 7+; supports && pipeline operator)"
        );
        return WindowsShell::Pwsh;
    }
    if exists("powershell.exe") {
        log::info!(
            "[aft] bash shell resolved to powershell.exe (Windows PowerShell 5.1; && in pipelines unsupported, will surface as parse error)"
        );
        return WindowsShell::Powershell;
    }
    // cmd.exe is always present on Windows. We log a warning because landing
    // here means BOTH PowerShell variants are missing, which is unusual — the
    // user is on a stripped Windows SKU like IoT LTSC, or their PATH is broken.
    log::warn!(
        "[aft] no PowerShell found on PATH (neither pwsh.exe nor powershell.exe); \
         falling back to cmd.exe — bash-style commands using && and || will work, \
         but PowerShell-only cmdlets will not. See https://github.com/cortexkit/aft/issues/27"
    );
    WindowsShell::Cmd
}

fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn cmd_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
