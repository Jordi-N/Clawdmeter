//! "Apagar el PC al agotar el límite": a one-shot option in the tray menu.
//! Armed by hand; the first fresh reading with the 5-hour session or the
//! week at 100 % schedules a shutdown in [`DELAY_SECS`] and disarms it. The
//! shutdown can be cancelled from the tray meanwhile.

use std::io;
use std::process::Command;

use crate::ipc::StatusSnapshot;

/// Seconds between the limit and the shutdown.
pub const DELAY_SECS: i64 = 60;

/// Which limit ran out, if any, in a reading taken after `armed_at`. A
/// missing, stale or older reading never counts: no data is not 100 %.
pub fn limit_hit(snap: &StatusSnapshot, armed_at: i64) -> Option<&'static str> {
    if snap.stale || snap.read_at.is_none_or(|t| t <= armed_at) {
        return None;
    }
    if snap.window_started && snap.session_pct.is_some_and(|p| p >= 100) {
        Some("la sesión de 5 horas")
    } else if snap.week_pct.is_some_and(|p| p >= 100) {
        Some("el límite semanal")
    } else {
        None
    }
}

/// A program and its arguments.
pub type OsCommand = (&'static str, Vec<String>);

/// The command that powers this computer off in [`DELAY_SECS`] (`None`: not
/// supported on this OS).
pub fn schedule_command(why: &str) -> Option<OsCommand> {
    if cfg!(windows) {
        let note = format!("Clawdmeter: se agotó {why}. El PC se apagará en 1 minuto.");
        Some(("shutdown", vec!["/s".into(), "/t".into(), DELAY_SECS.to_string(), "/c".into(), note]))
    } else if cfg!(target_os = "linux") {
        Some(("shutdown", vec!["-h".into(), "+1".into(), format!("Clawdmeter: se agotó {why}")]))
    } else {
        None
    }
}

/// The command that cancels a scheduled shutdown.
pub fn cancel_command() -> Option<OsCommand> {
    if cfg!(windows) {
        Some(("shutdown", vec!["/a".into()]))
    } else if cfg!(target_os = "linux") {
        Some(("shutdown", vec!["-c".into()]))
    } else {
        None
    }
}

/// Run one of the commands above (no console window on Windows).
pub fn run((program, args): OsCommand) -> io::Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(&args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{program} ended with {status}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARMED: i64 = 1_000;

    fn snap(session: Option<u32>, week: Option<u32>, read_at: i64) -> StatusSnapshot {
        StatusSnapshot {
            session_pct: session,
            week_pct: week,
            window_started: true,
            read_at: Some(read_at),
            ..Default::default()
        }
    }

    #[test]
    fn fires_on_a_fresh_reading_at_100() {
        assert_eq!(limit_hit(&snap(Some(100), Some(40), ARMED + 1), ARMED), Some("la sesión de 5 horas"));
        assert_eq!(limit_hit(&snap(Some(30), Some(100), ARMED + 1), ARMED), Some("el límite semanal"));
        assert_eq!(limit_hit(&snap(Some(99), Some(99), ARMED + 1), ARMED), None);
    }

    #[test]
    fn never_on_old_stale_or_missing_data() {
        // Read before it was armed.
        assert_eq!(limit_hit(&snap(Some(100), None, ARMED), ARMED), None);
        let stale = StatusSnapshot { stale: true, ..snap(Some(100), None, ARMED + 1) };
        assert_eq!(limit_hit(&stale, ARMED), None);
        let unread = StatusSnapshot { read_at: None, ..snap(Some(100), None, ARMED + 1) };
        assert_eq!(limit_hit(&unread, ARMED), None);
        // A session that hasn't started reports no live percentage.
        let idle = StatusSnapshot { window_started: false, ..snap(Some(100), Some(10), ARMED + 1) };
        assert_eq!(limit_hit(&idle, ARMED), None);
    }

    #[test]
    fn the_commands_wait_a_minute() {
        if let Some((program, args)) = schedule_command("el límite semanal") {
            assert_eq!(program, "shutdown");
            assert!(args.iter().any(|a| a == "60" || a == "+1"));
        }
        assert_eq!(cancel_command().is_some(), schedule_command("x").is_some());
    }
}
