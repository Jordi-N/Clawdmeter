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
    let repo = Settings::load(paths)
        .map(|s| s.release_repo)
        .unwrap_or_else(|_| clawdmeter_flash::DEFAULT_RELEASE_REPO.into());
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
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        // ping: about 3 s for the app to quit; `start /wait`: cmd doesn't wait
        // for a GUI program otherwise. raw_arg keeps cmd's own quoting.
        let script = format!(
            "/C ping -n 4 127.0.0.1 >nul & start \"\" /wait msiexec /i \"{}\" /passive /norestart & start \"\" \"{}\"",
            msi.display(),
            exe.display()
        );
        std::process::Command::new("cmd")
            .raw_arg(script)
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
