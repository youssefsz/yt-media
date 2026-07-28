//! Native desktop runtime for YT Media.

#![forbid(unsafe_code)]

/// Starts the native desktop application.
///
/// # Errors
///
/// Returns an error when Tauri cannot initialize or run the application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> Result<(), tauri::Error> {
    tauri::Builder::default().run(tauri::generate_context!())
}
