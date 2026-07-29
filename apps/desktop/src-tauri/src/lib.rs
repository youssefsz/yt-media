//! Native desktop runtime for YT Media.

#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tauri::{Emitter, Manager};

mod commands;
pub mod ipc;
mod service;

use ipc::{JOB_EVENT_NAME, JobEventEnvelopeDto};
use service::{ApplicationService, JobEventSink, ServiceConfig};

struct TauriEventSink {
    app: tauri::AppHandle,
}

impl JobEventSink for TauriEventSink {
    fn emit(&self, event: JobEventEnvelopeDto) {
        if let Err(error) = self.app.emit(JOB_EVENT_NAME, event) {
            eprintln!("could not emit desktop job event: {error:#}");
        }
    }
}

#[derive(Default)]
struct NativeLifecycle {
    shutdown_started: AtomicBool,
    exit_allowed: AtomicBool,
}

impl NativeLifecycle {
    fn begin_shutdown(&self) -> bool {
        self.shutdown_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn allow_exit(&self) {
        self.exit_allowed.store(true, Ordering::Release);
    }
}

/// Starts the native desktop application.
///
/// # Errors
///
/// Returns an error when Tauri cannot initialize or run the application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> Result<(), tauri::Error> {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            |app, _arguments, _working_directory| {
                if let Some(window) = app.get_webview_window("main") {
                    let _ignored = window.unminimize();
                    let _ignored = window.show();
                    let _ignored = window.set_focus();
                }
            },
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .setup(|app| {
            let data_directory = app.path().app_local_data_dir()?;
            let resource_directory = app.path().resource_dir()?;
            let sink = Arc::new(TauriEventSink {
                app: app.handle().clone(),
            });
            app.manage(ApplicationService::new(
                ServiceConfig::production(data_directory, &resource_directory),
                sink,
            ));
            app.manage(NativeLifecycle::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::analyze,
            commands::enqueue,
            commands::list_jobs,
            commands::get_job,
            commands::pause_job,
            commands::resume_job,
            commands::cancel_job,
            commands::retry_job,
            commands::list_history,
            commands::delete_history,
            commands::read_settings,
            commands::update_settings,
            commands::choose_destination,
            commands::reveal_output,
            commands::tool_status,
        ])
        .build(tauri::generate_context!())?;
    app.run(|app_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let lifecycle = app_handle.state::<NativeLifecycle>();
            if !lifecycle.exit_allowed.load(Ordering::Acquire) {
                api.prevent_exit();
                if lifecycle.begin_shutdown() {
                    let app_handle = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let service = app_handle.state::<ApplicationService>();
                        if let Err(error) = service.shutdown().await {
                            eprintln!("desktop shutdown completed with a diagnostic: {error:?}");
                        }
                        app_handle.state::<NativeLifecycle>().allow_exit();
                        app_handle.exit(0);
                    });
                }
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::Value;

    use crate::ipc::COMMAND_NAMES;

    #[test]
    fn capability_is_minimal_and_denies_general_native_access() {
        let capability: Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap_or_default();
        let permissions = capability
            .get("permissions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(permissions, vec![Value::String("core:default".to_owned())]);
        let serialized = capability.to_string();
        for forbidden in ["shell", "fs:", "dialog:", "opener:", "process", "execute"] {
            assert!(!serialized.contains(forbidden), "{forbidden} was exposed");
        }
    }

    #[test]
    fn command_registry_has_no_duplicates() {
        let unique = COMMAND_NAMES.into_iter().collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), COMMAND_NAMES.len());
    }

    #[test]
    fn shutdown_and_second_launch_coordination_are_idempotent() {
        let lifecycle = super::NativeLifecycle::default();
        assert!(lifecycle.begin_shutdown());
        assert!(!lifecycle.begin_shutdown());
        assert!(
            !lifecycle
                .exit_allowed
                .load(std::sync::atomic::Ordering::Acquire)
        );
        lifecycle.allow_exit();
        assert!(
            lifecycle
                .exit_allowed
                .load(std::sync::atomic::Ordering::Acquire)
        );
    }
}
