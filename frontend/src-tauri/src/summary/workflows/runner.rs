use crate::database::repositories::setting::SettingsRepository;
use crate::summary::llm_client::LLMProvider;
use crate::summary::processor::generate_meeting_summary;
use crate::summary::templates;
use crate::summary::workflows::models::{Workflow, WorkflowRunStatus};
use crate::summary::workflows::repository::WorkflowsRepository;
use crate::summary::workflows::sections::parse_sections;
use once_cell::sync::Lazy;
use reqwest::Client;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

// Per-run cancellation registry (mirrors summary::service CANCELLATION_REGISTRY).
static RUN_CANCELLATION: Lazy<Arc<Mutex<HashMap<String, CancellationToken>>>> =
    Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

fn register_cancel(run_id: &str) -> CancellationToken {
    let token = CancellationToken::new();
    if let Ok(mut reg) = RUN_CANCELLATION.lock() {
        reg.insert(run_id.to_string(), token.clone());
    }
    token
}

fn cleanup_cancel(run_id: &str) {
    if let Ok(mut reg) = RUN_CANCELLATION.lock() {
        reg.remove(run_id);
    }
}

pub fn cancel_run(run_id: &str) -> bool {
    if let Ok(reg) = RUN_CANCELLATION.lock() {
        if let Some(t) = reg.get(run_id) {
            t.cancel();
            return true;
        }
    }
    false
}

/// Pure: parse the markdown blob into ordered `{title, content}` and serialize.
pub(crate) fn build_sections_json(markdown: &str, section_titles: &[String]) -> String {
    let sections = parse_sections(markdown, section_titles);
    serde_json::to_string(&sections).unwrap_or_else(|_| "[]".to_string())
}

/// Fire-and-forget: generates the workflow summary and writes the run row.
pub async fn run_workflow_background<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    pool: SqlitePool,
    run_id: String,
    workflow: Workflow,
    _meeting_id: String,
    text: String,
    summary_language: Option<String>,
) {
    let token = register_cancel(&run_id);

    let result = generate_for_workflow(&app, &pool, &workflow, &text, summary_language, &token).await;

    match result {
        Ok((final_markdown, section_titles)) => {
            let sections_json = build_sections_json(&final_markdown, &section_titles);
            match WorkflowsRepository::complete_run(
                &pool, &run_id, &final_markdown, &sections_json, WorkflowRunStatus::COMPLETED,
            ).await {
                Ok(()) => {
                    // `workflow` is only borrowed by `generate_for_workflow` above, so
                    // it's still owned here — read its export config for the auto-export hook.
                    let cfg = workflow.neohive_config();
                    if cfg.enabled && cfg.auto_export {
                        if let Err(e) = crate::summary::workflows::commands::export_run(&pool, &run_id).await {
                            tracing::warn!("Auto-export to NeoHive failed for run {}: {}", run_id, e);
                        }
                    }

                    let obs = workflow.obsidian_config();
                    if obs.enabled && obs.auto_export {
                        // `save_run_to_obsidian` already marks the run `failed` itself at
                        // genuine filesystem-failure points; just log here.
                        if let Err(e) = crate::summary::workflows::commands::save_run_to_obsidian(&pool, &run_id).await {
                            tracing::warn!("Auto-save to Obsidian failed for run {}: {}", run_id, e);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to persist completed workflow run {}: {}", run_id, e);
                }
            }
        }
        Err(e) => {
            let status = if e.contains("cancelled") {
                WorkflowRunStatus::CANCELLED
            } else {
                WorkflowRunStatus::ERROR
            };
            if let Err(db_e) = WorkflowsRepository::fail_run(&pool, &run_id, &e, status).await {
                error!("Failed to persist failed workflow run {}: {}", run_id, db_e);
            }
        }
    }

    cleanup_cancel(&run_id);
}

/// Returns (final_markdown, ordered_section_titles).
async fn generate_for_workflow<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    pool: &SqlitePool,
    workflow: &Workflow,
    text: &str,
    summary_language: Option<String>,
    token: &CancellationToken,
) -> Result<(String, Vec<String>), String> {
    info!("Running workflow '{}' with {}/{}", workflow.name, workflow.provider, workflow.model);

    let provider = LLMProvider::from_str(&workflow.provider)?;
    let api_key = SettingsRepository::get_api_key(pool, &workflow.provider)
        .await
        .map_err(|e| format!("Failed to read API key: {}", e))?
        .unwrap_or_default();

    // Endpoints (ollama / custom-openai) come from saved model config.
    let (ollama_endpoint, custom_openai_endpoint) = resolve_endpoints(pool, &workflow.provider).await;

    let template = templates::get_template(&workflow.template_id)
        .map_err(|e| format!("Failed to load template '{}': {}", workflow.template_id, e))?;
    let section_titles: Vec<String> = template.sections.iter().map(|s| s.title.clone()).collect();

    let app_data_dir: Option<PathBuf> = app.path().app_data_dir().ok();

    let custom_prompt = workflow.custom_prompt.clone().unwrap_or_default();
    let vocab_cfg = SettingsRepository::get_vocabulary_config(pool).await.ok().flatten().unwrap_or_default();
    let custom_prompt = crate::vocabulary::prepend_glossary(custom_prompt, &vocab_cfg.glossary_entries(), 2000);
    let client = Client::new();

    let detected_transcript_language: Option<&str> = None; // Auto path; language detection handled elsewhere
    let summary = generate_meeting_summary(
        &client,
        &provider,
        &workflow.model,
        &api_key,
        text,
        &custom_prompt,
        &workflow.template_id,
        &template,
        40000, // token_threshold, mirrors api_process_transcript chunk_size default
        ollama_endpoint.as_deref(),
        custom_openai_endpoint.as_deref(),
        workflow.max_tokens.map(|v| v as u32),
        workflow.temperature.map(|v| v as f32),
        workflow.top_p.map(|v| v as f32),
        app_data_dir.as_ref(),
        Some(token),
        summary_language.as_deref(),
        detected_transcript_language,
        None, // cached_english
        None, // meeting_metadata_block
    )
    .await?;

    Ok((summary.final_markdown, section_titles))
}

/// Reads ollama endpoint + custom-openai endpoint from settings (best-effort).
async fn resolve_endpoints(pool: &SqlitePool, provider: &str) -> (Option<String>, Option<String>) {
    let mut ollama = None;
    let mut custom = None;
    if let Ok(Some(setting)) = SettingsRepository::get_model_config(pool).await {
        ollama = setting.ollama_endpoint.clone();
    }
    if provider == "custom-openai" {
        if let Ok(Some(cfg)) = SettingsRepository::get_custom_openai_config(pool).await {
            custom = Some(cfg.endpoint);
        }
    }
    (ollama, custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_sections_json_roundtrips_titles_and_content() {
        let md = "# T\n**Summary**\nhi\n**Action Items**\n- a\n";
        let titles = vec!["Summary".to_string(), "Action Items".to_string()];
        let json = build_sections_json(md, &titles);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
        assert_eq!(parsed[0]["title"], "Summary");
        assert!(parsed[0]["content"].as_str().unwrap().contains("hi"));
    }
}
