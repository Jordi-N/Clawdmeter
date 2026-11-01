//! Short, human error text for the windows. Technical detail goes to the
//! log, not the UI.

use std::io;
use std::path::Path;

/// One line for an I/O failure, without the OS error code.
pub fn io_message(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::PermissionDenied => "Sin permiso de escritura.".into(),
        io::ErrorKind::NotFound => "No se encontró el archivo o la carpeta.".into(),
        io::ErrorKind::ReadOnlyFilesystem => "El disco es de solo lectura.".into(),
        io::ErrorKind::StorageFull => "El disco está lleno.".into(),
        _ => {
            // Drop the " (os error 13)" suffix std adds.
            let s = e.to_string();
            match s.rfind(" (os error") {
                Some(i) => s[..i].to_owned(),
                None => s,
            }
        }
    }
}

/// "Error al guardar los ajustes (~/x/config). Sin permiso de escritura."
pub fn save_failed(what: &str, path: &Path, home: &Path, e: &io::Error) -> String {
    format!("Error al guardar {what} ({}). {}", crate::tilde(&path.display().to_string(), home), io_message(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        assert_eq!(io_message(&io::Error::from(io::ErrorKind::PermissionDenied)), "Sin permiso de escritura.");
        assert_eq!(io_message(&io::Error::from_raw_os_error(28)), "El disco está lleno.");
        let s = save_failed(
            "los ajustes",
            Path::new("/home/u/.config/x"),
            Path::new("/home/u"),
            &io::Error::from(io::ErrorKind::PermissionDenied),
        );
        assert_eq!(s, "Error al guardar los ajustes (~/.config/x). Sin permiso de escritura.");
    }
}
