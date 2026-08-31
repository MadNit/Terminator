//! terminator-core — headless session engine.
//!
//! Deliberately has **no UI dependencies**. Transports, the byte tap, logging,
//! storage and secrets all live here; the shell in front (Tauri today, possibly
//! Electron later) is a thin adapter that owns no session logic.
//!
//! Keeping this boundary means the webview decision stays reversible.

pub mod files;
pub mod known_hosts;
#[cfg(feature = "rdp")]
pub mod rdp;
pub mod secrets;
pub mod session;
pub mod shell_init;
pub mod store;
pub mod tap;
#[cfg(feature = "ssh")]
pub mod tunnels;
pub mod transport;
pub mod vault;

pub use known_hosts::{KnownHostEntry, KnownHostsManager};
pub use session::{LogPaths, SessionManager};
pub use store::Snippet;
#[cfg(feature = "ssh")]
pub use tunnels::{TunnelConfig, TunnelKind, TunnelManager, TunnelStatus};
pub use transport::TransportSpec;

/// The shell-integration snippet that enables per-command tracking.
///
/// OSC 133 is what turns a dumb byte stream into structured history: prompt,
/// command, output and exit code become explicit. This can be injected on SSH
/// connect so it works on servers that were never configured for it.
///
/// We report the command line explicitly via `133;E` rather than relying on
/// the `B..C` window and scraping the shell's echo. Scraping breaks on
/// multi-line prompts, right-hand prompts and syntax highlighting (p10k, zsh
/// autosuggestions), all of which rewrite the line after it is typed.
///
/// Both hook sets are additive -- clobbering `precmd`/`PROMPT_COMMAND` would
/// silently break whatever the user already had configured.
pub const OSC133_BASH_ZSH: &str = r#"
__term_osc()      { printf '\033]133;%s\007' "$1"; }
__term_cmd_line() { printf '\033]133;E;%s\007' "$1"; }
if [ -n "$ZSH_VERSION" ]; then
  __term_precmd()  { __term_osc "D;$?"; __term_osc A; }
  __term_preexec() { __term_cmd_line "$1"; __term_osc C; }
  autoload -Uz add-zsh-hook 2>/dev/null && {
    add-zsh-hook precmd  __term_precmd
    add-zsh-hook preexec __term_preexec
  }
elif [ -n "$BASH_VERSION" ]; then
  __term_precmd() { local e=$?; __term_osc "D;$e"; __term_ran=; __term_osc A; }
  __term_debug() {
    [ -n "$COMP_LINE" ] && return          # tab completion, not a command
    [ -n "$__term_ran" ] && return          # only the first command per prompt
    case "$BASH_COMMAND" in __term_*) return ;; esac
    __term_ran=1
    __term_cmd_line "$BASH_COMMAND"
    __term_osc C
  }
  PROMPT_COMMAND="__term_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
  trap '__term_debug' DEBUG
fi
"#;
