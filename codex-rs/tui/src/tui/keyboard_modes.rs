//! Terminal keyboard enhancement setup and teardown helpers.
//!
//! The TUI pairs pushes and pops on each screen's keyboard enhancement stack, but
//! process exit gets a stronger reset so the parent shell does not inherit enhanced key
//! reporting if a terminal misses the normal stack pop.

use std::fmt;
use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use codex_terminal_detection::TerminalName;
use codex_terminal_detection::terminal_info;
use crossterm::Command;
use crossterm::event::KeyboardEnhancementFlags;
use crossterm::event::PopKeyboardEnhancementFlags;
use crossterm::event::PushKeyboardEnhancementFlags;
use ratatui::crossterm::execute;

const DISABLE_KEYBOARD_ENHANCEMENT_ENV_VAR: &str = "CODEX_TUI_DISABLE_KEYBOARD_ENHANCEMENT";
static REPORTS_KEY_RELEASES: AtomicBool = AtomicBool::new(/*v*/ false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VscodeDetection {
    VsCode,
    Other,
    Unknown,
}

pub(super) fn keyboard_enhancement_disabled() -> bool {
    let disable_env = std::env::var(DISABLE_KEYBOARD_ENHANCEMENT_ENV_VAR).ok();
    let is_wsl = running_in_wsl();
    let vscode_detection = if is_wsl {
        detect_vscode_terminal()
    } else {
        VscodeDetection::Other
    };
    keyboard_enhancement_disabled_for(disable_env.as_deref(), is_wsl, vscode_detection)
}

fn keyboard_enhancement_disabled_for(
    disable_env: Option<&str>,
    is_wsl: bool,
    vscode_detection: VscodeDetection,
) -> bool {
    if let Some(disabled) = parse_bool_env(disable_env) {
        return disabled;
    }

    // VS Code running a WSL shell can hide TERM_PROGRAM from the Linux process
    // environment. Disable the keyboard mode that can break dead-key composition
    // whenever VS Code is detected on WSL.
    is_wsl
        && matches!(
            vscode_detection,
            VscodeDetection::VsCode | VscodeDetection::Unknown
        )
}

fn parse_bool_env(value: Option<&str>) -> Option<bool> {
    match value.map(str::trim) {
        Some("1") => Some(true),
        Some(value) if value.eq_ignore_ascii_case("true") => Some(true),
        Some(value) if value.eq_ignore_ascii_case("yes") => Some(true),
        Some("0") => Some(false),
        Some(value) if value.eq_ignore_ascii_case("false") => Some(false),
        Some(value) if value.eq_ignore_ascii_case("no") => Some(false),
        _ => None,
    }
}

fn running_in_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::clipboard_paste::is_probably_wsl()
    }

    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

pub(super) fn running_in_vscode_terminal() -> bool {
    detect_vscode_terminal() == VscodeDetection::VsCode
}

pub(crate) fn detect_vscode_terminal() -> VscodeDetection {
    if term_program_is_vscode(std::env::var("TERM_PROGRAM").ok().as_deref()) {
        VscodeDetection::VsCode
    } else {
        VscodeDetection::Other
    }
}

fn term_program_is_vscode(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case("vscode"))
}

/// Restore keyboard reporting and return the mouse policy from the same fresh tmux probe.
pub(super) fn enable_keyboard_enhancement(writer: &mut impl Write) -> super::tmux::MouseCapture {
    let tmux_options = super::tmux::options();
    if keyboard_enhancement_disabled() {
        set_reports_key_releases(/*reports*/ false);
        return tmux_options.mouse_capture;
    }

    let running_in_tmux_session = running_in_tmux_session();
    let tmux_extended_keys_format = if running_in_tmux_session {
        tmux_options.extended_keys_format.as_deref()
    } else {
        None
    };

    let flags = keyboard_enhancement_flags(
        terminal_info().name,
        running_in_tmux_session,
        tmux_extended_keys_format,
    );
    let result = execute!(
        writer,
        DisableModifyOtherKeys,
        PushKeyboardEnhancementFlags(flags)
    );
    set_reports_key_releases(
        result.is_ok() && flags.contains(KeyboardEnhancementFlags::REPORT_EVENT_TYPES),
    );

    if tmux_should_enable_modify_other_keys_for(running_in_tmux_session, tmux_extended_keys_format)
    {
        let _ = execute!(writer, EnableModifyOtherKeys);
    }
    tmux_options.mouse_capture
}

fn keyboard_enhancement_flags(
    terminal_name: TerminalName,
    running_in_tmux_session: bool,
    tmux_extended_keys_format: Option<&str>,
) -> KeyboardEnhancementFlags {
    let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS;

    // iTerm and Ghostty can leak shortcut release events that the terminal consumes.
    // tmux's xterm key format also loses Shift-Enter when event types are
    // reported. An unavailable/unrecognized tmux probe must take the same safe
    // fallback. Preserve repeat classification on confirmed csi-u transports.
    if matches!(terminal_name, TerminalName::Ghostty | TerminalName::Iterm2)
        || (running_in_tmux_session && !matches!(tmux_extended_keys_format, Some("csi-u")))
    {
        flags
    } else {
        flags | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
    }
}

pub(super) fn reports_key_releases() -> bool {
    REPORTS_KEY_RELEASES.load(Ordering::Relaxed)
}

pub(super) fn set_reports_key_releases(reports: bool) {
    REPORTS_KEY_RELEASES.store(reports, Ordering::Relaxed);
}

fn running_in_tmux_session() -> bool {
    tmux_session_detected(
        std::env::var("TMUX").ok().as_deref(),
        std::env::var("TMUX_PANE").ok().as_deref(),
    )
}

fn tmux_session_detected(tmux: Option<&str>, tmux_pane: Option<&str>) -> bool {
    tmux.is_some() || tmux_pane.is_some()
}

fn tmux_should_enable_modify_other_keys_for(
    running_in_tmux_session: bool,
    extended_keys_format: Option<&str>,
) -> bool {
    // Only request mode 2 when tmux confirms csi-u formatting. Older tmux
    // versions do not expose this option and may emit xterm-style sequences,
    // which crossterm does not parse consistently for modified keys.
    running_in_tmux_session && matches!(extended_keys_format, Some("csi-u"))
}

pub(super) fn restore_keyboard_enhancement_stack(writer: &mut impl Write) {
    if execute!(writer, PopKeyboardEnhancementFlags, DisableModifyOtherKeys).is_ok() {
        set_reports_key_releases(/*reports*/ false);
    }
}

pub(super) fn reset_keyboard_reporting_after_exit(writer: &mut impl Write) {
    if execute!(
        writer,
        PopKeyboardEnhancementFlags,
        ResetKeyboardEnhancementFlags,
        DisableModifyOtherKeys
    )
    .is_ok()
    {
        set_reports_key_releases(/*reports*/ false);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResetKeyboardEnhancementFlags;

impl Command for ResetKeyboardEnhancementFlags {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[<u")
    }

}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnableModifyOtherKeys;

impl Command for EnableModifyOtherKeys {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[>4;2m")
    }

}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DisableModifyOtherKeys;

impl Command for DisableModifyOtherKeys {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[>4;0m")
    }

}

#[cfg(test)]
mod tests {
    use super::DisableModifyOtherKeys;
    use super::EnableModifyOtherKeys;
    use super::ResetKeyboardEnhancementFlags;
    use super::VscodeDetection;
    use super::keyboard_enhancement_disabled_for;
    use super::keyboard_enhancement_flags;
    use super::parse_bool_env;
    use super::tmux_session_detected;
    use super::tmux_should_enable_modify_other_keys_for;
    use codex_terminal_detection::TerminalName;
    use crossterm::Command;
    use crossterm::event::PushKeyboardEnhancementFlags;
    use pretty_assertions::assert_eq;

    fn ansi_for(command: impl Command) -> String {
        let mut out = String::new();
        command.write_ansi(&mut out).unwrap();
        out
    }

    #[test]
    fn keyboard_enhancement_suppresses_release_reporting_for_iterm() {
        assert_eq!(
            ansi_for(PushKeyboardEnhancementFlags(keyboard_enhancement_flags(
                TerminalName::Iterm2,
                /*running_in_tmux_session*/ false,
                /*tmux_extended_keys_format*/ None
            ))),
            "\x1b[>5u"
        );
    }

    #[test]
    fn keyboard_enhancement_suppresses_release_reporting_for_ghostty() {
        assert_eq!(
            ansi_for(PushKeyboardEnhancementFlags(keyboard_enhancement_flags(
                TerminalName::Ghostty,
                /*running_in_tmux_session*/ false,
                /*tmux_extended_keys_format*/ None
            ))),
            "\x1b[>5u"
        );
    }

    #[test]
    fn keyboard_enhancement_preserves_repeat_reporting_for_kitty() {
        assert_eq!(
            ansi_for(PushKeyboardEnhancementFlags(keyboard_enhancement_flags(
                TerminalName::Kitty,
                /*running_in_tmux_session*/ false,
                /*tmux_extended_keys_format*/ None
            ))),
            "\x1b[>7u"
        );
    }

    #[test]
    fn keyboard_enhancement_preserves_repeat_reporting_for_csi_u_tmux() {
        assert_eq!(
            ansi_for(PushKeyboardEnhancementFlags(keyboard_enhancement_flags(
                TerminalName::Kitty,
                /*running_in_tmux_session*/ true,
                Some("csi-u")
            ))),
            "\x1b[>7u"
        );
    }

    #[test]
    fn keyboard_enhancement_preserves_shift_enter_for_xterm_tmux() {
        assert_eq!(
            ansi_for(PushKeyboardEnhancementFlags(keyboard_enhancement_flags(
                TerminalName::Kitty,
                /*running_in_tmux_session*/ true,
                Some("xterm")
            ))),
            "\x1b[>5u"
        );
    }

    #[test]
    fn keyboard_enhancement_preserves_repeat_reporting_for_unknown_terminals() {
        assert_eq!(
            ansi_for(PushKeyboardEnhancementFlags(keyboard_enhancement_flags(
                TerminalName::Unknown,
                /*running_in_tmux_session*/ false,
                /*tmux_extended_keys_format*/ None
            ))),
            "\x1b[>7u"
        );
    }

    #[test]
    fn keyboard_enhancement_uses_conservative_flags_when_tmux_format_is_unknown() {
        assert_eq!(
            ansi_for(PushKeyboardEnhancementFlags(keyboard_enhancement_flags(
                TerminalName::Unknown,
                /*running_in_tmux_session*/ true,
                /*tmux_extended_keys_format*/ None,
            ))),
            "\x1b[>5u"
        );
    }

    #[test]
    fn keyboard_enhancement_env_flag_parses_common_values() {
        assert_eq!(parse_bool_env(Some("1")), Some(true));
        assert_eq!(parse_bool_env(Some("true")), Some(true));
        assert_eq!(parse_bool_env(Some("YES")), Some(true));
        assert_eq!(parse_bool_env(Some("0")), Some(false));
        assert_eq!(parse_bool_env(Some("false")), Some(false));
        assert_eq!(parse_bool_env(Some("NO")), Some(false));
        assert_eq!(parse_bool_env(Some("unexpected")), None);
        assert_eq!(parse_bool_env(/*value*/ None), None);
    }

    #[test]
    fn keyboard_enhancement_auto_disables_for_vscode_or_unknown_in_wsl() {
        for detection in [
            VscodeDetection::VsCode,
            VscodeDetection::Other,
            VscodeDetection::Unknown,
        ] {
            assert_eq!(
                keyboard_enhancement_disabled_for(
                    /*disable_env*/ None, /*is_wsl*/ true, detection
                ),
                detection != VscodeDetection::Other,
            );
            assert!(!keyboard_enhancement_disabled_for(
                /*disable_env*/ None, /*is_wsl*/ false, detection
            ));
        }
    }

    #[test]
    fn keyboard_enhancement_env_flag_overrides_auto_detection() {
        for detection in [
            VscodeDetection::VsCode,
            VscodeDetection::Other,
            VscodeDetection::Unknown,
        ] {
            assert!(!keyboard_enhancement_disabled_for(
                Some("0"),
                /*is_wsl*/ true,
                detection
            ));
            assert!(keyboard_enhancement_disabled_for(
                Some("1"),
                /*is_wsl*/ false,
                detection
            ));
        }
    }

    #[test]
    fn tmux_session_detection_accepts_tmux_or_tmux_pane() {
        assert!(tmux_session_detected(
            Some("/tmp/tmux-501/default,1,0"),
            /*tmux_pane*/ None
        ));
        assert!(tmux_session_detected(/*tmux*/ None, Some("%0")));
        assert!(!tmux_session_detected(
            /*tmux*/ None, /*tmux_pane*/ None
        ));
    }

    #[test]
    fn tmux_modify_other_keys_only_requests_confirmed_csi_u_format() {
        assert!(tmux_should_enable_modify_other_keys_for(
            /*running_in_tmux_session*/ true,
            Some("csi-u")
        ));
        assert!(!tmux_should_enable_modify_other_keys_for(
            /*running_in_tmux_session*/ true, /*extended_keys_format*/ None
        ));
        assert!(!tmux_should_enable_modify_other_keys_for(
            /*running_in_tmux_session*/ true,
            Some("xterm")
        ));
        assert!(!tmux_should_enable_modify_other_keys_for(
            /*running_in_tmux_session*/ true,
            Some("")
        ));
        assert!(!tmux_should_enable_modify_other_keys_for(
            /*running_in_tmux_session*/ false,
            Some("csi-u")
        ));
    }

    #[test]
    fn reset_keyboard_enhancement_flags_clears_all_pushed_levels() {
        assert_eq!(ansi_for(ResetKeyboardEnhancementFlags), "\x1b[<u");
    }

    #[test]
    fn enable_modify_other_keys_requests_xterm_keyboard_reporting() {
        assert_eq!(ansi_for(EnableModifyOtherKeys), "\x1b[>4;2m");
    }

    #[test]
    fn disable_modify_other_keys_resets_xterm_keyboard_reporting() {
        assert_eq!(ansi_for(DisableModifyOtherKeys), "\x1b[>4;0m");
    }
}
