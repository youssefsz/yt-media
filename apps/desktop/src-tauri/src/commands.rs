//! Thin Tauri transport adapters over [`crate::service::ApplicationService`].

use tauri::{AppHandle, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::{
    ipc::{
        ActionResultDto, AnalyzeRequestDto, AnalyzeResponseDto, BootstrapStateDto,
        DestinationSelectionDto, EnqueueRequestDto, IPC_SCHEMA_VERSION, IpcErrorCodeDto,
        IpcErrorDto, JobDto, JobIdRequestDto, SettingsDto, ToolStatusDto, UpdateCheckResultDto,
        UpdateSettingsRequestDto,
    },
    service::ApplicationService,
};

/// Returns recovered authoritative startup state.
#[tauri::command]
pub async fn bootstrap(
    service: State<'_, ApplicationService>,
) -> Result<BootstrapStateDto, IpcErrorDto> {
    Ok(service.bootstrap().await)
}

/// Analyzes one validated public URL.
#[tauri::command]
pub async fn analyze(
    service: State<'_, ApplicationService>,
    request: AnalyzeRequestDto,
) -> Result<AnalyzeResponseDto, IpcErrorDto> {
    service.analyze(request).await
}

/// Cancels the currently active URL analysis, when one exists.
#[tauri::command]
pub async fn cancel_analysis(
    service: State<'_, ApplicationService>,
) -> Result<ActionResultDto, IpcErrorDto> {
    service.cancel_analysis().await;
    Ok(action_result())
}

/// Enqueues one validated download request.
#[tauri::command]
pub async fn enqueue(
    service: State<'_, ApplicationService>,
    request: EnqueueRequestDto,
) -> Result<JobDto, IpcErrorDto> {
    service.enqueue(request).await
}

/// Lists durable jobs.
#[tauri::command]
pub async fn list_jobs(service: State<'_, ApplicationService>) -> Result<Vec<JobDto>, IpcErrorDto> {
    service.list_jobs().await
}

/// Reads one durable job.
#[tauri::command]
pub async fn get_job(
    service: State<'_, ApplicationService>,
    request: JobIdRequestDto,
) -> Result<JobDto, IpcErrorDto> {
    service.get_job(request).await
}

/// Pauses one job.
#[tauri::command]
pub async fn pause_job(
    service: State<'_, ApplicationService>,
    request: JobIdRequestDto,
) -> Result<JobDto, IpcErrorDto> {
    service.pause_job(request).await
}

/// Resumes one job explicitly.
#[tauri::command]
pub async fn resume_job(
    service: State<'_, ApplicationService>,
    request: JobIdRequestDto,
) -> Result<JobDto, IpcErrorDto> {
    service.resume_job(request).await
}

/// Cancels one job.
#[tauri::command]
pub async fn cancel_job(
    service: State<'_, ApplicationService>,
    request: JobIdRequestDto,
) -> Result<JobDto, IpcErrorDto> {
    service.cancel_job(request).await
}

/// Retries one stopped terminal job explicitly.
#[tauri::command]
pub async fn retry_job(
    service: State<'_, ApplicationService>,
    request: JobIdRequestDto,
) -> Result<JobDto, IpcErrorDto> {
    service.retry_job(request).await
}

/// Lists terminal history.
#[tauri::command]
pub async fn list_history(
    service: State<'_, ApplicationService>,
) -> Result<Vec<JobDto>, IpcErrorDto> {
    service.list_history().await
}

/// Deletes one completed history record without deleting output.
#[tauri::command]
pub async fn delete_history(
    service: State<'_, ApplicationService>,
    request: JobIdRequestDto,
) -> Result<ActionResultDto, IpcErrorDto> {
    service.delete_history(request).await?;
    Ok(action_result())
}

/// Reads persisted engine settings.
#[tauri::command]
pub async fn read_settings(
    service: State<'_, ApplicationService>,
) -> Result<SettingsDto, IpcErrorDto> {
    service.read_settings().await
}

/// Validates and persists settings.
#[tauri::command]
pub async fn update_settings(
    service: State<'_, ApplicationService>,
    request: UpdateSettingsRequestDto,
) -> Result<SettingsDto, IpcErrorDto> {
    service.update_settings(request).await
}

/// Opens the operating system's native folder picker.
#[tauri::command]
pub async fn choose_destination(
    window: WebviewWindow,
) -> Result<DestinationSelectionDto, IpcErrorDto> {
    let picker = window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("Choose download destination");
    let selection = tauri::async_runtime::spawn_blocking(move || picker.blocking_pick_folder())
        .await
        .map_err(|_| {
            IpcErrorDto::new(
                IpcErrorCodeDto::DestinationSelectionFailed,
                "The native folder picker could not complete.",
            )
        })?;
    let path = selection
        .map(|selection| {
            selection.into_path().map_err(|_| {
                IpcErrorDto::new(
                    IpcErrorCodeDto::DestinationSelectionFailed,
                    "The selected folder is not available as a local filesystem path.",
                )
            })
        })
        .transpose()?
        .map(|path| path.to_string_lossy().chars().take(4_096).collect());
    Ok(DestinationSelectionDto { path })
}

/// Reveals one engine-validated completed output in the native file manager.
#[tauri::command]
pub async fn reveal_output(
    app: AppHandle,
    service: State<'_, ApplicationService>,
    request: JobIdRequestDto,
) -> Result<ActionResultDto, IpcErrorDto> {
    let path = service.revealable_output(request).await?;
    app.opener().reveal_item_in_dir(path).map_err(|error| {
        eprintln!("native output reveal failed: {error:#}");
        IpcErrorDto::new(
            IpcErrorCodeDto::RevealFailed,
            "The completed output could not be revealed in the system file manager.",
        )
    })?;
    Ok(action_result())
}

/// Returns current verified tool status.
#[tauri::command]
pub async fn tool_status(
    service: State<'_, ApplicationService>,
) -> Result<Vec<ToolStatusDto>, IpcErrorDto> {
    Ok(service.tool_status().await)
}

/// Checks, downloads, verifies, health checks, and activates a newer signed tool set.
#[tauri::command]
pub async fn check_for_tool_updates(
    service: State<'_, ApplicationService>,
) -> Result<UpdateCheckResultDto, IpcErrorDto> {
    service.check_for_tool_updates().await
}

/// Removes managed tool sets so resolution falls back to the immutable bundled baseline.
#[tauri::command]
pub async fn reset_tool_updates(
    service: State<'_, ApplicationService>,
) -> Result<ActionResultDto, IpcErrorDto> {
    service.reset_tool_updates().await?;
    Ok(action_result())
}

fn action_result() -> ActionResultDto {
    ActionResultDto {
        schema_version: IPC_SCHEMA_VERSION,
    }
}
