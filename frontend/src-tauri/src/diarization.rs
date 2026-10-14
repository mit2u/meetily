use std::path::PathBuf;

use sherpa_rs::diarize::{Diarize, DiarizeConfig};
use tauri::{AppHandle, Manager, Runtime};

use crate::audio::decoder::decode_audio_file;
use crate::audio::recording_preferences::get_default_recordings_folder;
use crate::state::AppState;

/// Label each transcript segment of a meeting with a speaker ("Speaker 1", "Speaker 2", ...).
#[tauri::command]
pub async fn diarize_meeting<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<usize, String> {
    let pool = state.db_manager.pool();

    let folder: String = sqlx::query_scalar::<_, Option<String>>("SELECT folder_path FROM meetings WHERE id = ?")
        .bind(&meeting_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?
        .flatten()
        .ok_or("Meeting has no recording folder")?;
    let mut folder = PathBuf::from(folder);
    if folder.is_relative() {
        folder = get_default_recordings_folder().join(folder);
    }

    let models = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("models/diarization");

    let segments = tauri::async_runtime::spawn_blocking(move || {
        let samples = decode_audio_file(&folder.join("audio.mp4"))
            .map_err(|e| e.to_string())?
            .to_whisper_format();
        let threads = std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4);
        let mut diarizer = Diarize::new(
            models.join("segmentation-3.0-sherpa.onnx"),
            models.join("wespeaker-resnet34-LM-sherpa.onnx"),
            DiarizeConfig { threshold: Some(0.5), num_threads: Some(threads), ..Default::default() },
        )
        .map_err(|e| e.to_string())?;
        diarizer.compute(samples, None).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    let rows: Vec<(String, f64, f64)> = sqlx::query_as(
        "SELECT id, audio_start_time, audio_end_time FROM transcripts
         WHERE meeting_id = ? AND audio_start_time IS NOT NULL AND audio_end_time IS NOT NULL",
    )
    .bind(&meeting_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let mut labelled = 0;
    for (id, start, end) in rows {
        // Pick the speaker whose segments overlap this transcript line the most
        let best = segments
            .iter()
            .map(|s| (s.speaker, (end.min(s.end as f64) - start.max(s.start as f64)).max(0.0)))
            .filter(|(_, overlap)| *overlap > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((speaker, _)) = best {
            sqlx::query("UPDATE transcripts SET speaker = ? WHERE id = ?")
                .bind(format!("Speaker {}", speaker + 1))
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
            labelled += 1;
        }
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(labelled)
}
