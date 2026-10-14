use crate::database::models::MeetingModel;
use crate::database::repositories::meeting::MeetingsRepository;
use crate::database::repositories::setting::SettingsRepository;
use crate::neohive::NeoHiveClient;
use crate::state::AppState;
use crate::summary::workflows::models::{
    NeoHiveExportConfig, ObsidianExportConfig, Workflow, WorkflowInput, WorkflowRun, WorkflowRunStatus,
};
use crate::summary::workflows::repository::WorkflowsRepository;
use crate::summary::workflows::runner;
use crate::summary::workflows::sections::{memory_type_for, ParsedSection};
use log::{error as log_error, info as log_info};
use serde::Serialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Runtime};

#[tauri::command]
pub async fn api_list_workflows(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Workflow>, String> {
    log_info!("api_list_workflows called");
    WorkflowsRepository::list_workflows(state.db_manager.pool())
        .await
        .map_err(|e| {
            log_error!("api_list_workflows failed: {}", e);
            e.to_string()
        })
}

#[tauri::command]
pub async fn api_save_workflow(
    state: tauri::State<'_, AppState>,
    workflow: WorkflowInput,
) -> Result<Workflow, String> {
    log_info!("api_save_workflow called (name: {})", workflow.name);
    if workflow.name.trim().is_empty() {
        return Err("Workflow name cannot be empty".to_string());
    }
    if workflow.provider.trim().is_empty() || workflow.model.trim().is_empty() {
        return Err("Workflow provider and model are required".to_string());
    }
    WorkflowsRepository::upsert_workflow(state.db_manager.pool(), &workflow)
        .await
        .map_err(|e| {
            log_error!("api_save_workflow failed: {}", e);
            e.to_string()
        })
}

#[tauri::command]
pub async fn api_delete_workflow(
    state: tauri::State<'_, AppState>,
    workflow_id: String,
) -> Result<bool, String> {
    log_info!("api_delete_workflow called (id: {})", workflow_id);
    WorkflowsRepository::delete_workflow(state.db_manager.pool(), &workflow_id)
        .await
        .map_err(|e| {
            log_error!("api_delete_workflow failed: {}", e);
            e.to_string()
        })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartedResponse {
    pub run_id: String,
}

#[tauri::command]
pub async fn api_run_workflow<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    workflow_id: String,
    meeting_id: String,
    text: String,
    summary_language: Option<String>,
) -> Result<RunStartedResponse, String> {
    log_info!("api_run_workflow called (workflow {}, meeting {})", workflow_id, meeting_id);

    let pool = state.db_manager.pool().clone();

    let workflow = WorkflowsRepository::get_workflow(&pool, &workflow_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Workflow '{}' not found", workflow_id))?;

    if text.trim().is_empty() {
        return Err("No transcript text available for this meeting".to_string());
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    WorkflowsRepository::create_run(&pool, &run_id, Some(&workflow.id), &workflow.name, &meeting_id)
        .await
        .map_err(|e| e.to_string())?;

    let summary_language = summary_language.and_then(|s| {
        let t = s.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    });

    let run_id_spawn = run_id.clone();
    tauri::async_runtime::spawn(async move {
        runner::run_workflow_background(
            app, pool, run_id_spawn, workflow, meeting_id, text, summary_language,
        ).await;
    });

    Ok(RunStartedResponse { run_id })
}

#[tauri::command]
pub async fn api_get_workflow_run(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<Option<WorkflowRun>, String> {
    WorkflowsRepository::get_run(state.db_manager.pool(), &run_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn api_list_workflow_runs(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<WorkflowRun>, String> {
    WorkflowsRepository::list_runs_for_meeting(state.db_manager.pool(), &meeting_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn api_cancel_workflow_run(run_id: String) -> Result<bool, String> {
    log_info!("api_cancel_workflow_run called (run {})", run_id);
    Ok(runner::cancel_run(&run_id))
}

/// One memory to push: (content, memory_type, tags).
#[derive(Debug, PartialEq)]
pub struct ExportItem {
    pub content: String,
    pub mem_type: String,
    pub tags: Vec<String>,
}

/// Pure: turns parsed sections + config + context into export items (skips empty sections).
pub(crate) fn build_export_items(
    sections: &[ParsedSection],
    cfg: &NeoHiveExportConfig,
    meeting_title: &str,
    workflow_name: &str,
) -> Vec<ExportItem> {
    sections
        .iter()
        .filter(|s| !s.content.trim().is_empty())
        .map(|s| ExportItem {
            content: format!("{}\n\n{}", s.title, s.content),
            mem_type: memory_type_for(&s.title, cfg),
            tags: vec![
                meeting_title.to_string(),
                workflow_name.to_string(),
                s.title.clone(),
                "meetily".to_string(),
            ],
        })
        .collect()
}

/// Double-quotes a string for a YAML scalar, escaping backslashes and quotes.
fn yaml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Strips filesystem-hostile characters; collapses whitespace; never empty.
pub(crate) fn sanitize_filename(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') && !c.is_control())
        .collect();
    // collapse internal whitespace runs to single spaces, trim ends
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed: String = collapsed.trim_matches('.').trim().chars().take(120).collect();
    if trimmed.is_empty() || trimmed.chars().all(|c| c == '.' || c.is_whitespace()) {
        "meeting".to_string()
    } else {
        trimmed
    }
}

/// Builds (filename, file_contents) for a completed run. Frontmatter uses the
/// meeting's created_at for both `createdAt` (YYYY-MM-DD, matching the vault's
/// standup convention) and `date` (RFC3339). `attendees` is omitted when empty.
pub(crate) fn build_obsidian_note(
    run: &WorkflowRun,
    meeting: &MeetingModel,
    attendees: &[String],
    cfg: &ObsidianExportConfig,
) -> (String, String) {
    let created = meeting.created_at.0;
    let day = created.format("%Y-%m-%d").to_string();
    let filename = format!("{} - {}.md", day, sanitize_filename(&meeting.title));

    let mut tags: Vec<String> = vec!["meeting".to_string(), "meetily".to_string()];
    for t in &cfg.tags {
        let t = t.trim();
        if !t.is_empty() && !tags.contains(&t.to_string()) {
            tags.push(t.to_string());
        }
    }
    let tags_yaml = tags.iter().map(|t| yaml_str(t)).collect::<Vec<_>>().join(", ");

    let mut fm = String::from("---\n");
    fm.push_str(&format!("title: {}\n", yaml_str(&meeting.title)));
    fm.push_str(&format!("createdAt: {}\n", day));
    fm.push_str(&format!("date: {}\n", created.to_rfc3339()));
    if !attendees.is_empty() {
        let att = attendees.iter().map(|a| yaml_str(a)).collect::<Vec<_>>().join(", ");
        fm.push_str(&format!("attendees: [{}]\n", att));
    }
    fm.push_str(&format!("tags: [{}]\n", tags_yaml));
    fm.push_str(&format!("meetingId: {}\n", run.meeting_id));
    fm.push_str("---\n\n");

    let body = run.result_markdown.clone().unwrap_or_default();
    let contents = format!("{}# {}\n\n{}\n", fm, meeting.title, body);
    (filename, contents)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub pushed: usize,
    pub failed: usize,
}

/// neohive_status label given attempted export counts (pushed + failed >= 1).
pub(crate) fn neohive_status_label(pushed: usize, failed: usize) -> &'static str {
    if failed == 0 { "pushed" } else if pushed == 0 { "failed" } else { "partial" }
}

/// Exports a completed run's sections to NeoHive. Shared by the manual command
/// and the auto-export hook. Sends over the Cloudflare Access service token.
pub(crate) async fn export_run(pool: &SqlitePool, run_id: &str) -> Result<ExportResult, String> {
    let run = WorkflowsRepository::get_run(pool, run_id)
        .await.map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Run '{}' not found", run_id))?;
    if run.status != WorkflowRunStatus::COMPLETED {
        return Err("Only completed runs can be exported".to_string());
    }

    let sections: Vec<ParsedSection> = run.result_sections.as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    // Note: this never trips for a real workflow — `parse_sections` always emits
    // one entry per template section (empty content for missing ones). The real
    // "anything to export" guard is the post-filter `items.is_empty()` check below.
    if sections.is_empty() {
        return Err("This run has no sections to export".to_string());
    }

    let neo = SettingsRepository::get_neohive_config(pool).await.map_err(|e| e.to_string())?;
    if !neo.enabled {
        return Err("NeoHive export is disabled in Settings".to_string());
    }
    let endpoint = neo.endpoint.ok_or("NeoHive endpoint is not configured")?;
    let auth_config_val: serde_json::Value = neo.auth_config.as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);
    let auth = crate::neohive::NeoHiveAuth::from_parts(neo.auth_type.as_deref(), &auth_config_val)?;

    // Per-workflow export config (type overrides, importance).
    let export_cfg = match &run.workflow_id {
        Some(id) => WorkflowsRepository::get_workflow(pool, id).await.ok().flatten()
            .map(|w| w.neohive_config()).unwrap_or_default(),
        None => NeoHiveExportConfig::default(),
    };

    // Meeting title for tagging. `MeetingsRepository::get_meeting_by_id` (the brief's
    // guess) does not exist. The real lightweight getter (no transcripts joined) is
    // `get_meeting_metadata`, returning `Result<Option<MeetingModel>, sqlx::Error>`
    // where `MeetingModel.title: String`.
    let meeting_title = MeetingsRepository::get_meeting_metadata(pool, &run.meeting_id)
        .await.ok().flatten().map(|m| m.title).unwrap_or_else(|| "Meeting".to_string());

    let items = build_export_items(&sections, &export_cfg, &meeting_title, &run.workflow_name);
    if items.is_empty() {
        return Err("This run produced no non-empty sections to export".to_string());
    }

    let client = NeoHiveClient::new(endpoint, auth);

    let mut pushed = 0usize;
    let mut failed = 0usize;
    for item in &items {
        match client.store_memory(&item.content, &item.mem_type, &item.tags, export_cfg.importance).await {
            Ok(()) => pushed += 1,
            Err(e) => { log_error!("NeoHive export item failed: {}", e); failed += 1; }
        }
    }

    let status = neohive_status_label(pushed, failed);
    if let Err(e) = WorkflowsRepository::set_run_neohive_status(pool, run_id, status).await {
        log_error!("Failed to update neohive_status for run {}: {}", run_id, e);
    }

    Ok(ExportResult { pushed, failed })
}

#[tauri::command]
pub async fn api_export_run_to_neohive(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<ExportResult, String> {
    log_info!("api_export_run_to_neohive called (run {})", run_id);
    export_run(state.db_manager.pool(), &run_id).await
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeoHiveConfigResponse {
    pub endpoint: Option<String>,
    pub enabled: bool,
    pub auth_type: Option<String>,
    pub auth_config: Option<serde_json::Value>,
}

#[tauri::command]
pub async fn api_get_neohive_config(
    state: tauri::State<'_, AppState>,
) -> Result<NeoHiveConfigResponse, String> {
    log_info!("api_get_neohive_config called");
    let cfg = SettingsRepository::get_neohive_config(state.db_manager.pool())
        .await
        .map_err(|e| { log_error!("api_get_neohive_config failed: {}", e); e.to_string() })?;
    let auth_config = cfg.auth_config.as_deref().and_then(|s| serde_json::from_str(s).ok());
    Ok(NeoHiveConfigResponse {
        endpoint: cfg.endpoint,
        enabled: cfg.enabled,
        auth_type: cfg.auth_type,
        auth_config,
    })
}

#[tauri::command]
pub async fn api_save_neohive_config(
    state: tauri::State<'_, AppState>,
    endpoint: Option<String>,
    enabled: bool,
    auth_type: Option<String>,
    auth_config: Option<serde_json::Value>,
) -> Result<(), String> {
    log_info!("api_save_neohive_config called (enabled={}, authType={:?})", enabled, auth_type);
    let auth_config_str = auth_config.map(|v| v.to_string());
    SettingsRepository::save_neohive_config(
        state.db_manager.pool(),
        endpoint.as_deref(),
        enabled,
        auth_type.as_deref(),
        auth_config_str.as_deref(),
    )
    .await
    .map_err(|e| { log_error!("api_save_neohive_config failed: {}", e); e.to_string() })
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsidianConfigResponse {
    pub vault_path: Option<String>,
    pub enabled: bool,
}

#[tauri::command]
pub async fn api_get_obsidian_config(
    state: tauri::State<'_, AppState>,
) -> Result<ObsidianConfigResponse, String> {
    log_info!("api_get_obsidian_config called");
    let cfg = SettingsRepository::get_obsidian_config(state.db_manager.pool())
        .await
        .map_err(|e| { log_error!("api_get_obsidian_config failed: {}", e); e.to_string() })?;
    Ok(ObsidianConfigResponse { vault_path: cfg.vault_path, enabled: cfg.enabled })
}

#[tauri::command]
pub async fn api_save_obsidian_config(
    state: tauri::State<'_, AppState>,
    vault_path: Option<String>,
    enabled: bool,
) -> Result<(), String> {
    log_info!("api_save_obsidian_config called (enabled={})", enabled);
    SettingsRepository::save_obsidian_config(state.db_manager.pool(), vault_path.as_deref(), enabled)
        .await
        .map_err(|e| { log_error!("api_save_obsidian_config failed: {}", e); e.to_string() })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsidianSaveResult {
    pub path: String,
}

/// Writes a completed run's markdown to the configured Obsidian vault folder.
/// Shared by the manual command and the auto-save hook. Overwrites an existing
/// same-day-same-title note by design (one clean note per meeting).
pub(crate) async fn save_run_to_obsidian(
    pool: &SqlitePool,
    run_id: &str,
) -> Result<ObsidianSaveResult, String> {
    let run = WorkflowsRepository::get_run(pool, run_id)
        .await.map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Run '{}' not found", run_id))?;
    if run.status != WorkflowRunStatus::COMPLETED {
        return Err("Only completed runs can be saved to Obsidian".to_string());
    }

    let obs = SettingsRepository::get_obsidian_config(pool).await.map_err(|e| e.to_string())?;
    if !obs.enabled {
        return Err("Obsidian export is disabled in Settings".to_string());
    }
    let vault = obs.vault_path.filter(|p| !p.trim().is_empty())
        .ok_or("Obsidian vault path is not configured")?;
    let vault_path = std::path::PathBuf::from(&vault);
    if !vault_path.is_dir() {
        return Err(format!("Obsidian vault path does not exist or is not a directory: {}", vault));
    }

    // Per-workflow config (subfolder, tags); default if workflow was deleted.
    let cfg: ObsidianExportConfig = match &run.workflow_id {
        Some(id) => WorkflowsRepository::get_workflow(pool, id).await.ok().flatten()
            .map(|w| w.obsidian_config()).unwrap_or_default(),
        None => ObsidianExportConfig::default(),
    };

    let meeting = MeetingsRepository::get_meeting_metadata(pool, &run.meeting_id)
        .await.map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Meeting '{}' not found", run.meeting_id))?;
    let attendees = MeetingsRepository::get_distinct_named_speakers(pool, &run.meeting_id)
        .await.unwrap_or_default();

    let (filename, contents) = build_obsidian_note(&run, &meeting, &attendees, &cfg);

    // Resolve target dir (vault [+ subfolder]). Reject an unsafe subfolder up
    // front (before touching the filesystem) so an escaping value like
    // "../Foo" or an absolute path never gets a directory created for it.
    let sub_opt = cfg.subfolder.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(sub) = sub_opt {
        let sub_path = std::path::Path::new(sub);
        let has_parent_dir = sub_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        if sub_path.is_absolute() || has_parent_dir {
            // Precondition failure (no fs write attempted) — leave obsidian_status alone.
            return Err(format!(
                "Obsidian subfolder must be a relative path without '..': {}",
                sub
            ));
        }
    }
    let target_dir = match sub_opt {
        Some(sub) => vault_path.join(sub),
        None => vault_path.clone(),
    };
    if let Err(e) = std::fs::create_dir_all(&target_dir) {
        if let Err(set_e) = WorkflowsRepository::set_run_obsidian_result(pool, run_id, "failed", None).await {
            log_error!("Failed to update obsidian_status for run {}: {}", run_id, set_e);
        }
        return Err(format!("Failed to create folder: {}", e));
    }
    let vault_canon = std::fs::canonicalize(&vault_path).map_err(|e| e.to_string())?;
    let target_canon = std::fs::canonicalize(&target_dir).map_err(|e| e.to_string())?;
    if !target_canon.starts_with(&vault_canon) {
        if let Err(set_e) = WorkflowsRepository::set_run_obsidian_result(pool, run_id, "failed", None).await {
            log_error!("Failed to update obsidian_status for run {}: {}", run_id, set_e);
        }
        return Err("Resolved Obsidian path escapes the vault folder".to_string());
    }

    let file_path = target_canon.join(&filename);
    if let Err(e) = std::fs::write(&file_path, contents) {
        if let Err(set_e) = WorkflowsRepository::set_run_obsidian_result(pool, run_id, "failed", None).await {
            log_error!("Failed to update obsidian_status for run {}: {}", run_id, set_e);
        }
        return Err(format!("Failed to write note: {}", e));
    }
    let path_str = file_path.to_string_lossy().to_string();

    if let Err(e) = WorkflowsRepository::set_run_obsidian_result(pool, run_id, "saved", Some(&path_str)).await {
        log_error!("Failed to update obsidian_status for run {}: {}", run_id, e);
    }
    Ok(ObsidianSaveResult { path: path_str })
}

#[tauri::command]
pub async fn api_save_run_to_obsidian(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<ObsidianSaveResult, String> {
    log_info!("api_save_run_to_obsidian called (run {})", run_id);
    // `save_run_to_obsidian` marks the run `failed` itself at the genuine
    // filesystem-failure points; benign precondition errors (disabled, no
    // vault, not completed, unsafe subfolder) leave obsidian_status untouched.
    save_run_to_obsidian(state.db_manager.pool(), &run_id).await
}

#[cfg(test)]
mod tests {
    use crate::database::models::{MeetingModel, DateTimeUtc};
    use crate::database::repositories::meeting::MeetingsRepository;
    use crate::summary::workflows::models::{NeoHiveExportConfig, WorkflowInput, WorkflowRun};
    use crate::summary::workflows::models::ObsidianExportConfig;
    use crate::summary::workflows::repository::WorkflowsRepository;
    use chrono::TimeZone;
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;

    fn sample_meeting() -> MeetingModel {
        MeetingModel {
            id: "m1".into(),
            title: "Sprint Planning: Q3".into(),
            created_at: DateTimeUtc(chrono::Utc.with_ymd_and_hms(2026, 7, 15, 14, 30, 0).unwrap()),
            updated_at: DateTimeUtc(chrono::Utc.with_ymd_and_hms(2026, 7, 15, 15, 0, 0).unwrap()),
            folder_path: None,
            calendar_event_id: None,
            calendar_attendees: None,
            calendar_meet_link: None,
            calendar_start_time: None,
            calendar_end_time: None,
        }
    }

    fn sample_run() -> WorkflowRun {
        WorkflowRun {
            id: "r1".into(), workflow_id: Some("w1".into()), workflow_name: "Comprehensive".into(),
            meeting_id: "m1".into(), status: "completed".into(),
            result_markdown: Some("## Overview\nWe planned Q3.".into()),
            result_sections: None, error: None,
            neohive_status: "none".into(), obsidian_status: "none".into(), obsidian_path: None,
            created_at: chrono::Utc::now(), updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn sanitize_filename_strips_illegal_and_never_empty() {
        assert_eq!(super::sanitize_filename("a/b:c*?\"<>|d"), "abcd");
        assert_eq!(super::sanitize_filename("../../etc/passwd"), "etcpasswd");
        assert_eq!(super::sanitize_filename("   "), "meeting");
        assert!(!super::sanitize_filename("normal title").is_empty());
    }

    #[test]
    fn sanitize_filename_falls_back_for_dot_and_whitespace_titles() {
        assert_eq!(super::sanitize_filename(". . ."), "meeting");
        assert_eq!(super::sanitize_filename(". . . . ."), "meeting");
        assert_eq!(super::sanitize_filename("..a.."), "a");
    }

    #[test]
    fn build_obsidian_note_has_frontmatter_and_filename() {
        let cfg = ObsidianExportConfig { tags: vec!["planning".into()], ..Default::default() };
        let (filename, contents) = super::build_obsidian_note(&sample_run(), &sample_meeting(), &["Alice".into(), "Bob".into()], &cfg);
        assert_eq!(filename, "2026-07-15 - Sprint Planning Q3.md");
        assert!(contents.contains("createdAt: 2026-07-15"));
        assert!(contents.contains("title:"));
        assert!(contents.contains("meetingId: m1"));
        assert!(contents.contains("attendees: [\"Alice\", \"Bob\"]"));
        assert!(contents.contains("\"meeting\""));
        assert!(contents.contains("\"meetily\""));
        assert!(contents.contains("\"planning\""));
        assert!(contents.contains("We planned Q3."));
        assert!(contents.starts_with("---\n"));
    }

    #[test]
    fn build_obsidian_note_trims_and_dedups_config_tags() {
        let cfg = ObsidianExportConfig { tags: vec![" planning ".into(), "meeting".into()], ..Default::default() };
        let (_f, contents) = super::build_obsidian_note(&sample_run(), &sample_meeting(), &[], &cfg);
        assert!(contents.contains("\"planning\""));
        assert!(!contents.contains(" planning \""));
        assert!(!contents.contains("\"meeting\", \"meetily\", \"meeting\""));
        let meeting_tag_count = contents.matches("\"meeting\"").count();
        assert_eq!(meeting_tag_count, 1);
    }

    #[test]
    fn build_obsidian_note_omits_attendees_when_empty() {
        let (_f, contents) = super::build_obsidian_note(&sample_run(), &sample_meeting(), &[], &ObsidianExportConfig::default());
        assert!(!contents.contains("attendees:"));
    }

    #[test]
    fn neohive_status_label_maps_counts() {
        assert_eq!(super::neohive_status_label(3, 0), "pushed");
        assert_eq!(super::neohive_status_label(0, 2), "failed");
        assert_eq!(super::neohive_status_label(2, 1), "partial");
    }

    #[test]
    fn build_export_items_skips_empty_and_maps_types() {
        use crate::summary::workflows::sections::ParsedSection;
        use crate::summary::workflows::models::NeoHiveExportConfig;
        use std::collections::HashMap;
        let mut overrides = HashMap::new();
        overrides.insert("Key Decisions".to_string(), "decision".to_string());
        let cfg = NeoHiveExportConfig { section_type_overrides: overrides, ..Default::default() };
        let sections = vec![
            ParsedSection { title: "Summary".into(), content: "hi".into() },
            ParsedSection { title: "Key Decisions".into(), content: "  ".into() }, // empty -> skipped
        ];
        let items = super::build_export_items(&sections, &cfg, "Sprint", "Exec");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].mem_type, "narrative");
        assert!(items[0].tags.contains(&"meetily".to_string()));
        assert!(items[0].content.starts_with("Summary"));
    }

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new().max_connections(1)
            .connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn save_then_list_then_delete_roundtrip() {
        let pool = test_pool().await;
        let input = WorkflowInput {
            id: None, name: "Exec".into(), description: None,
            template_id: "standard_meeting".into(), custom_prompt: None,
            provider: "openrouter".into(), model: "x/y".into(),
            max_tokens: None, temperature: None, top_p: None,
            neohive_export: Some(NeoHiveExportConfig::default()),
            obsidian_export: None,
        };
        let wf = WorkflowsRepository::upsert_workflow(&pool, &input).await.unwrap();
        assert_eq!(WorkflowsRepository::list_workflows(&pool).await.unwrap().len(), 1);
        assert!(WorkflowsRepository::delete_workflow(&pool, &wf.id).await.unwrap());
        assert_eq!(WorkflowsRepository::list_workflows(&pool).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn create_run_shows_in_meeting_run_list() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1','T','t','t')")
            .execute(&pool).await.unwrap();
        WorkflowsRepository::create_run(&pool, "run1", None, "W", "m1").await.unwrap();
        let runs = WorkflowsRepository::list_runs_for_meeting(&pool, "m1").await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "queued");
        assert_eq!(runs[0].neohive_status, "none");
    }

    #[tokio::test]
    async fn save_run_to_obsidian_writes_file_and_sets_status() {
        let pool = test_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().to_str().unwrap();

        crate::database::repositories::setting::SettingsRepository::save_obsidian_config(&pool, Some(vault), true).await.unwrap();
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1','Sync','2026-07-15T10:00:00Z','2026-07-15T10:00:00Z')")
            .execute(&pool).await.unwrap();

        let input = crate::summary::workflows::models::WorkflowInput {
            id: None, name: "Comp".into(), description: None,
            template_id: "comprehensive_meeting".into(), custom_prompt: None,
            provider: "openrouter".into(), model: "x/y".into(),
            max_tokens: None, temperature: None, top_p: None,
            neohive_export: None,
            obsidian_export: Some(crate::summary::workflows::models::ObsidianExportConfig {
                enabled: true, auto_export: true, subfolder: None, tags: vec![],
            }),
        };
        let wf = WorkflowsRepository::upsert_workflow(&pool, &input).await.unwrap();
        WorkflowsRepository::create_run(&pool, "r1", Some(&wf.id), &wf.name, "m1").await.unwrap();
        WorkflowsRepository::complete_run(&pool, "r1", "## Overview\nhi", "[]", "completed").await.unwrap();

        let res = super::save_run_to_obsidian(&pool, "r1").await.unwrap();
        assert!(res.path.ends_with(".md"));
        assert!(std::path::Path::new(&res.path).exists());
        let run = WorkflowsRepository::get_run(&pool, "r1").await.unwrap().unwrap();
        assert_eq!(run.obsidian_status, "saved");
        assert_eq!(run.obsidian_path.as_deref(), Some(res.path.as_str()));
    }

    #[tokio::test]
    async fn save_run_to_obsidian_errors_when_disabled() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1','Sync','2026-07-15T10:00:00Z','2026-07-15T10:00:00Z')")
            .execute(&pool).await.unwrap();
        WorkflowsRepository::create_run(&pool, "r1", None, "W", "m1").await.unwrap();
        WorkflowsRepository::complete_run(&pool, "r1", "x", "[]", "completed").await.unwrap();
        let err = super::save_run_to_obsidian(&pool, "r1").await.unwrap_err();
        assert!(err.to_lowercase().contains("obsidian"));
        // Precondition error (Obsidian disabled) — must NOT flip obsidian_status to "failed".
        let run = WorkflowsRepository::get_run(&pool, "r1").await.unwrap().unwrap();
        assert_eq!(run.obsidian_status, "none");
    }

    #[tokio::test]
    async fn save_run_to_obsidian_rejects_escaping_subfolder_without_creating_it() {
        let pool = test_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().to_str().unwrap();

        crate::database::repositories::setting::SettingsRepository::save_obsidian_config(&pool, Some(vault), true).await.unwrap();
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1','Sync','2026-07-15T10:00:00Z','2026-07-15T10:00:00Z')")
            .execute(&pool).await.unwrap();

        let input = crate::summary::workflows::models::WorkflowInput {
            id: None, name: "Comp".into(), description: None,
            template_id: "comprehensive_meeting".into(), custom_prompt: None,
            provider: "openrouter".into(), model: "x/y".into(),
            max_tokens: None, temperature: None, top_p: None,
            neohive_export: None,
            obsidian_export: Some(crate::summary::workflows::models::ObsidianExportConfig {
                enabled: true, auto_export: true, subfolder: Some("../escape".into()), tags: vec![],
            }),
        };
        let wf = WorkflowsRepository::upsert_workflow(&pool, &input).await.unwrap();
        WorkflowsRepository::create_run(&pool, "r1", Some(&wf.id), &wf.name, "m1").await.unwrap();
        WorkflowsRepository::complete_run(&pool, "r1", "## Overview\nhi", "[]", "completed").await.unwrap();

        let err = super::save_run_to_obsidian(&pool, "r1").await.unwrap_err();
        assert!(err.to_lowercase().contains("subfolder"));

        // No directory should have been created outside the vault tempdir.
        let escape_dir = dir.path().parent().unwrap().join("escape");
        assert!(!escape_dir.exists());

        // Precondition rejection (no fs write attempted) — must NOT flip obsidian_status.
        let run = WorkflowsRepository::get_run(&pool, "r1").await.unwrap().unwrap();
        assert_eq!(run.obsidian_status, "none");
    }

    #[tokio::test]
    async fn get_distinct_named_speakers_excludes_placeholders_and_source_labels() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m1','Sync','2026-07-15T10:00:00Z','2026-07-15T10:00:00Z')")
            .execute(&pool).await.unwrap();
        for (i, speaker) in ["Speaker 1", "mic", "system", "Alice"].iter().enumerate() {
            sqlx::query(
                "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, speaker) VALUES (?, 'm1', 'hi', '2026-07-15T10:00:00Z', ?)",
            )
            .bind(format!("t{}", i))
            .bind(speaker)
            .execute(&pool)
            .await
            .unwrap();
        }

        let speakers = MeetingsRepository::get_distinct_named_speakers(&pool, "m1").await.unwrap();
        assert_eq!(speakers, vec!["Alice".to_string()]);
    }
}
