//! Updating the app itself to the release the daily check found. Windows:
//! download the release's `.msi`, check it against `SHA256SUMS`, then a
//! detached `cmd` waits for the app to quit, installs over it (the MSI's
//! MajorUpgrade) and starts it again. Elsewhere the page links the release.

use clawdmeter_core::{Paths, Settings};
use clawdmeter_flash::manifest::is_newer;
use clawdmeter_flash::release;

/// Whether `latest` (a release tag) is newer than this app. Local builds
/// whose version isn't a release number are never offered one.
pub fn offered(latest: &str) -> bool {
    crate::version().starts_with(|c: char| c.is_ascii_digit()) && is_newer(latest, crate::version())
}

/// Download and launch the update; on `Ok` (the new version) the caller
/// quits the app so the installer can replace it. Blocking.
pub fn install(paths: &Paths) -> Result<String, String> {
    let repo =
        Settings::load(paths).map(|s| s.release_repo).unwrap_or_else(|_| clawdmeter_flash::DEFAULT_RELEASE_REPO.into());
    let r = release::release_for_app(&repo, crate::version()).map_err(|e| e.to_string())?;
    if !offered(&r.tag) {
        return Err("Ya tienes la última versión.".into());
    }
    imp::launch(paths, &r)?;
    Ok(r.tag)
}

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;

    pub fn launch(paths: &clawdmeter_core::Paths, r: &clawdmeter_flash::Release) -> Result<(), String> {
        let msi = clawdmeter_flash::release::fetch_installer(r, &paths.cache_dir).map_err(|e| e.to_string())?;
        // The MSI installs per user here (tools/packaging/wix): start that
        // copy afterwards, not a .zip copy this might be running from.
        let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA no está definido")?;
        let exe = std::path::PathBuf::from(local).join("Programs").join("Clawdmeter").join("clawdmeter.exe");
        let log = paths.cache_dir.join("update-msi.log");
        // A script file rather than a command line: no quoting games. It
        // waits (up to 30 s) for every Clawdmeter process to quit, installs
        // with a log (update-msi.log, for when it fails), then starts the app.
        let q = |p: &std::path::Path| p.display().to_string().replace('\'', "''");
        let script = format!(
            "$ErrorActionPreference = 'SilentlyContinue'\r\n\
             Get-Process clawdmeter | Wait-Process -Timeout 30\r\n\
             Start-Process msiexec -Wait -ArgumentList @('/i', '\"{}\"', '/passive', '/norestart', '/l*v', '\"{}\"')\r\n\
             Start-Process '{}'\r\n",
            q(&msi),
            q(&log),
            q(&exe)
        );
        let ps1 = paths.cache_dir.join("update.ps1");
        std::fs::write(&ps1, script).map_err(|e| format!("No se pudo preparar la actualización: {e}"))?;
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
            .arg(&ps1)
            .creation_flags(0x0800_0000 | 0x0000_0200) // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
            .spawn()
            .map_err(|e| format!("No se pudo lanzar el instalador: {e}"))?;
        log::info!("self-update: installing {} after quitting", msi.display());
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn launch(_: &clawdmeter_core::Paths, r: &clawdmeter_flash::Release) -> Result<(), String> {
        Err(format!(
            "La actualización automática es solo para Windows. Descarga {} desde la página de releases.",
            r.tag
        ))
    }
}
