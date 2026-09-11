# antislop-cli

CLI & TUI UX design guidelines for terminal applications.

## Standard Terminal Output
- **stdout vs stderr**: Machine-readable or requested data belongs on `stdout`. Diagnostic messages, logs, progress bars, and warnings belong on `stderr`.
- **Machine Readability**: Provide `--json` or unformatted plain output modes when commands are likely to be piped or used in scripts.
- **Exit Codes**: Zero on success, non-zero on failure. Keep exit codes semantic (1 = general error, 2 = usage error / bad flag, 130 = SIGINT).

## TUI Layout & Ergonomics (Ratatui / ANSI)
- **Responsive Terminal Resizing**: Handle terminal resize events gracefully; avoid panicking or hard-wrapping when terminal width < 80 columns.
- **Polite Alternate Screen**: Use alternate screen buffers for full-screen TUIs and clean up the terminal state (cursor restoration, mouse capture release) on exit or panic.
- **Scroll & Bounds**: Never let terminal output runaway. Use bounded scrolling, truncating, or pagination.
