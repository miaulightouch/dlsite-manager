//! Thin commands over `dm-launcher-export`; launcher formats live in that crate.
use crate::{
    canonical_open_roots, canonicalize_existing_path, command_error, normalize_required_id,
    path_is_under_any_root, record_audit, AppState,
};
use dm_audit::AuditEvent;
use dm_launcher_export::{
    export_work, is_supported, prepare_options, ExportEnv, ExportOptions, ExportReport,
    ExportRequest, TargetStatus, WorkRef,
};
use dm_storage::WorkDownloadStatus;
use serde::Deserialize;
use serde_json::json;
use tauri::{AppHandle, State};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherExportOptionsRequest {
    work_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportWorkToLaunchersRequest {
    work_id: String,
    export: ExportRequest,
}

#[tauri::command]
pub fn launcher_export_supported() -> bool {
    is_supported()
}

/// Resolve the work from the library so the frontend never supplies its folder.
async fn export_context(
    app: &AppHandle,
    state: &AppState,
    work_id: String,
) -> Result<(ExportEnv, WorkRef), String> {
    if !is_supported() {
        return Err("Launcher export is only available on Linux".to_owned());
    }
    let work_id = normalize_required_id(work_id)?;
    let env = ExportEnv::from_process()
        .ok_or_else(|| "HOME is not set; cannot find launchers".to_owned())?;
    let download = state
        .storage
        .work_download_state(&work_id)
        .await
        .map_err(command_error)?;
    if download.status != WorkDownloadStatus::Downloaded {
        return Err(format!("{work_id} is not downloaded"));
    }
    let local_path = download
        .local_path
        .as_deref()
        .ok_or_else(|| format!("{work_id} does not have a local path"))?;
    let root = canonicalize_existing_path(local_path)?;
    let settings = state.storage.app_settings().await.map_err(command_error)?;
    if !path_is_under_any_root(&root, &canonical_open_roots(app, &settings)) {
        return Err(format!(
            "download path is outside the configured library or staging folders: {}",
            root.display()
        ));
    }
    let detail = state
        .library
        .product_detail(&work_id)
        .await
        .map_err(command_error)?;
    let other_work_roots = state
        .storage
        .work_download_paths()
        .await
        .map_err(command_error)?
        .into_iter()
        .filter(|(other, _)| *other != work_id)
        .map(|(_, path)| path.into())
        .collect();
    Ok((
        env,
        WorkRef {
            work_id,
            root,
            title: detail.title,
            cover_url: detail.thumbnail_url,
            other_work_roots,
        },
    ))
}

#[tauri::command]
pub async fn get_launcher_export_options(
    app: AppHandle,
    state: State<'_, AppState>,
    request: LauncherExportOptionsRequest,
) -> Result<ExportOptions, String> {
    let operation = state.audit.operation("get_launcher_export_options");
    operation
        .command(async {
            let (env, work) = export_context(&app, &state, request.work_id).await?;
            tauri::async_runtime::spawn_blocking(move || prepare_options(&env, &work))
                .await
                .map_err(command_error)?
                .map_err(|error| error.to_string())
        })
        .await
}

#[tauri::command]
pub async fn export_work_to_launchers(
    app: AppHandle,
    state: State<'_, AppState>,
    request: ExportWorkToLaunchersRequest,
) -> Result<ExportReport, String> {
    let operation = state.audit.operation("export_work_to_launchers");
    operation
        .command(async {
            let work_id = request.work_id.clone();
            let result = async {
                let (env, work) = export_context(&app, &state, request.work_id).await?;
                let export = request.export;
                tauri::async_runtime::spawn_blocking(move || export_work(&env, &work, &export))
                    .await
                    .map_err(command_error)?
                    .map_err(|error| error.to_string())
            }
            .await;
            record_audit(&state.audit, audit_event(&work_id, &result)).await;
            result
        })
        .await
}

fn audit_event(work_id: &str, result: &Result<ExportReport, String>) -> AuditEvent {
    match result {
        Ok(report) => {
            let targets: Vec<_> = report
                .results
                .iter()
                .map(|target| {
                    json!({
                        "launcher": target.launcher,
                        "status": target.status,
                        "error": target.error,
                        "files": target.files,
                    })
                })
                .collect();
            let details = json!({ "workId": work_id, "prefix": report.prefix, "targets": targets });
            if report
                .results
                .iter()
                .any(|target| target.status == TargetStatus::Failed)
            {
                AuditEvent::failed("work.launcherExport", "Launcher export had failures")
                    .with_error(Some("launcher_export"), "One or more launchers failed")
                    .with_details(details)
            } else {
                AuditEvent::succeeded("work.launcherExport", "Exported work to launchers")
                    .with_details(details)
            }
        }
        Err(message) => AuditEvent::failed("work.launcherExport", "Launcher export failed")
            .with_error(Some("launcher_export"), message.clone())
            .with_details(json!({ "workId": work_id })),
    }
}
