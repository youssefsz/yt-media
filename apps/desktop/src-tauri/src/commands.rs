//! Thin Tauri transport adapters over [`crate::service::ApplicationService`].

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::oneshot;

use crate::{
    ipc::{
        ActionResultDto, AnalyzeRequestDto, AnalyzeResponseDto, BootstrapStateDto,
        DestinationSelectionDto, EnqueueRequestDto, IPC_SCHEMA_VERSION, IpcErrorCodeDto,
        IpcErrorDto, JobDto, JobIdRequestDto, SettingsDto, ToolStatusDto, UpdateSettingsRequestDto,
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
pub async fn choose_destination(app: AppHandle) -> Result<DestinationSelectionDto, IpcErrorDto> {
    let (sender, receiver) = oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose download destination")
        .pick_folder(move |selection| {
            let _ignored = sender.send(selection);
        });
    let selection = receiver.await.map_err(|_| {
        IpcErrorDto::new(
            IpcErrorCodeDto::DestinationSelectionFailed,
            "The native folder picker closed unexpectedly.",
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

fn action_result() -> ActionResultDto {
    ActionResultDto {
        schema_version: IPC_SCHEMA_VERSION,
    }
}
