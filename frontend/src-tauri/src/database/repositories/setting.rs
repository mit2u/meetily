use crate::database::models::{Setting, TranscriptSetting};
use crate::summary::CustomOpenAIConfig;
use crate::vocabulary::VocabularyConfig;
use sqlx::SqlitePool;

#[derive(serde::Deserialize, Debug)]
pub struct SaveModelConfigRequest {
    pub provider: String,
    pub model: String,
    #[serde(rename = "whisperModel")]
    pub whisper_model: String,
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
    #[serde(rename = "ollamaEndpoint")]
    pub ollama_endpoint: Option<String>,
}

#[derive(serde::Deserialize, Debug)]
pub struct SaveTranscriptConfigRequest {
    pub provider: String,
    pub model: String,
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
}

pub struct SettingsRepository;

#[derive(Debug, Clone, Default)]
pub struct NeoHiveSettings {
    pub endpoint: Option<String>,
    pub enabled: bool,
    pub auth_type: Option<String>,
    pub auth_config: Option<String>, // JSON string of method fields
}

#[derive(Debug, Clone, Default)]
pub struct ObsidianSettings {
    pub vault_path: Option<String>,
    pub enabled: bool,
}

// Transcript providers: localWhisper, deepgram, elevenLabs, groq, openai
// Summary providers: openai, claude, ollama, groq, added openrouter
// NOTE: Handle data exclusion in the higher layer as this is database abstraction layer(using SELECT *)

impl SettingsRepository {
    pub async fn get_model_config(
        pool: &SqlitePool,
    ) -> std::result::Result<Option<Setting>, sqlx::Error> {
        let setting = sqlx::query_as::<_, Setting>("SELECT * FROM settings LIMIT 1")
            .fetch_optional(pool)
            .await?;
        Ok(setting)
    }

    pub async fn save_model_config(
        pool: &SqlitePool,
        provider: &str,
        model: &str,
        whisper_model: &str,
        ollama_endpoint: Option<&str>,
    ) -> std::result::Result<(), sqlx::Error> {
        // Using id '1' for backward compatibility
        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, ollamaEndpoint)
            VALUES ('1', $1, $2, $3, $4)
            ON CONFLICT(id) DO UPDATE SET
                provider = excluded.provider,
                model = excluded.model,
                whisperModel = excluded.whisperModel,
                ollamaEndpoint = excluded.ollamaEndpoint
            "#,
        )
        .bind(provider)
        .bind(model)
        .bind(whisper_model)
        .bind(ollama_endpoint)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn save_api_key(
        pool: &SqlitePool,
        provider: &str,
        api_key: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        // Custom OpenAI uses JSON config (customOpenAIConfig) instead of a separate API key column
        if provider == "custom-openai" {
            return Err(sqlx::Error::Protocol(
                "custom-openai provider should use save_custom_openai_config() instead of save_api_key()".into(),
            ));
        }

        let api_key_column = match provider {
            "openai" => "openaiApiKey",
            "claude" => "anthropicApiKey",
            "ollama" => "ollamaApiKey",
            "groq" => "groqApiKey",
            "openrouter" => "openRouterApiKey",
            "builtin-ai" => return Ok(()), // No API key needed
            _ => {
                return Err(sqlx::Error::Protocol(
                    format!("Invalid provider: {}", provider).into(),
                ))
            }
        };

        let query = format!(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, "{}")
            VALUES ('1', 'openai', 'gpt-4o-2024-11-20', 'large-v3', $1)
            ON CONFLICT(id) DO UPDATE SET
                "{}" = $1
            "#,
            api_key_column, api_key_column
        );
        sqlx::query(&query).bind(api_key).execute(pool).await?;

        Ok(())
    }

    pub async fn get_api_key(
        pool: &SqlitePool,
        provider: &str,
    ) -> std::result::Result<Option<String>, sqlx::Error> {
        // Custom OpenAI uses JSON config - extract API key from there
        if provider == "custom-openai" {
            let config = Self::get_custom_openai_config(pool).await?;
            return Ok(config.and_then(|c| c.api_key));
        }

        let api_key_column = match provider {
            "openai" => "openaiApiKey",
            "ollama" => "ollamaApiKey",
            "groq" => "groqApiKey",
            "claude" => "anthropicApiKey",
            "openrouter" => "openRouterApiKey",
            "builtin-ai" => return Ok(None), // No API key needed
            _ => {
                return Err(sqlx::Error::Protocol(
                    format!("Invalid provider: {}", provider).into(),
                ))
            }
        };

        let query = format!(
            "SELECT {} FROM settings WHERE id = '1' LIMIT 1",
            api_key_column
        );
        let api_key = sqlx::query_scalar(&query).fetch_optional(pool).await?;
        Ok(api_key)
    }

    pub async fn get_transcript_config(
        pool: &SqlitePool,
    ) -> std::result::Result<Option<TranscriptSetting>, sqlx::Error> {
        let setting =
            sqlx::query_as::<_, TranscriptSetting>("SELECT * FROM transcript_settings LIMIT 1")
                .fetch_optional(pool)
                .await?;
        Ok(setting)

    }

    pub async fn save_transcript_config(
        pool: &SqlitePool,
        provider: &str,
        model: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO transcript_settings (id, provider, model)
            VALUES ('1', $1, $2)
            ON CONFLICT(id) DO UPDATE SET
                provider = excluded.provider,
                model = excluded.model
            "#,
        )
        .bind(provider)
        .bind(model)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn save_transcript_api_key(
        pool: &SqlitePool,
        provider: &str,
        api_key: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        let api_key_column = match provider {
            "localWhisper" => "whisperApiKey",
            "parakeet" => return Ok(()), // Parakeet doesn't need an API key, return early
            "deepgram" => "deepgramApiKey",
            "elevenLabs" => "elevenLabsApiKey",
            "groq" => "groqApiKey",
            "openai" => "openaiApiKey",
            "openrouter" => "openrouterApiKey",
            "custom" => "customApiKey",
            _ => {
                return Err(sqlx::Error::Protocol(
                    format!("Invalid provider: {}", provider).into(),
                ))
            }
        };

        let query = format!(
            r#"
            INSERT INTO transcript_settings (id, provider, model, "{}")
            VALUES ('1', 'parakeet', '{}', $1)
            ON CONFLICT(id) DO UPDATE SET
                "{}" = $1
            "#,
            api_key_column, crate::config::DEFAULT_PARAKEET_MODEL, api_key_column
        );
        sqlx::query(&query).bind(api_key).execute(pool).await?;

        Ok(())
    }

    pub async fn get_transcript_api_key(
        pool: &SqlitePool,
        provider: &str,
    ) -> std::result::Result<Option<String>, sqlx::Error> {
        let api_key_column = match provider {
            "localWhisper" => "whisperApiKey",
            "parakeet" => return Ok(None), // Parakeet doesn't need an API key
            "deepgram" => "deepgramApiKey",
            "elevenLabs" => "elevenLabsApiKey",
            "groq" => "groqApiKey",
            "openai" => "openaiApiKey",
            "openrouter" => "openrouterApiKey",
            "custom" => "customApiKey",
            _ => {
                return Err(sqlx::Error::Protocol(
                    format!("Invalid provider: {}", provider).into(),
                ))
            }
        };

        let query = format!(
            "SELECT {} FROM transcript_settings WHERE id = '1' LIMIT 1",
            api_key_column
        );
        let api_key = sqlx::query_scalar(&query).fetch_optional(pool).await?;
        Ok(api_key)
    }

    pub async fn save_transcript_base_url(
        pool: &SqlitePool,
        base_url: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO transcript_settings (id, provider, model, transcriptBaseUrl)
            VALUES ('1', 'custom', '', $1)
            ON CONFLICT(id) DO UPDATE SET
                transcriptBaseUrl = excluded.transcriptBaseUrl
            "#,
        )
        .bind(base_url)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn delete_api_key(
        pool: &SqlitePool,
        provider: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        // Custom OpenAI uses JSON config - clear the entire config
        if provider == "custom-openai" {
            sqlx::query("UPDATE settings SET customOpenAIConfig = NULL WHERE id = '1'")
                .execute(pool)
                .await?;
            return Ok(());
        }

        let api_key_column = match provider {
            "openai" => "openaiApiKey",
            "ollama" => "ollamaApiKey",
            "groq" => "groqApiKey",
            "claude" => "anthropicApiKey",
            "openrouter" => "openRouterApiKey",
            "builtin-ai" => return Ok(()), // No API key needed
            _ => {
                return Err(sqlx::Error::Protocol(
                    format!("Invalid provider: {}", provider).into(),
                ))
            }
        };

        let query = format!(
            "UPDATE settings SET {} = NULL WHERE id = '1'",
            api_key_column
        );
        sqlx::query(&query).execute(pool).await?;

        Ok(())
    }

    // ===== CUSTOM OPENAI CONFIG METHODS =====

    /// Gets the custom OpenAI configuration from JSON
    ///
    /// # Returns
    /// * `Ok(Some(CustomOpenAIConfig))` - Config exists and is valid JSON
    /// * `Ok(None)` - No config stored
    /// * `Err(sqlx::Error)` - Database error
    pub async fn get_custom_openai_config(
        pool: &SqlitePool,
    ) -> std::result::Result<Option<CustomOpenAIConfig>, sqlx::Error> {
        use sqlx::Row;

        let row = sqlx::query(
            r#"
            SELECT customOpenAIConfig
            FROM settings
            WHERE id = '1'
            LIMIT 1
            "#
        )
        .fetch_optional(pool)
        .await?;

        match row {
            Some(record) => {
                let config_json: Option<String> = record.get("customOpenAIConfig");

                if let Some(json) = config_json {
                    // Parse JSON into CustomOpenAIConfig
                    let config: CustomOpenAIConfig = serde_json::from_str(&json)
                        .map_err(|e| sqlx::Error::Protocol(
                            format!("Invalid JSON in customOpenAIConfig: {}", e).into()
                        ))?;

                    Ok(Some(config))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    /// Saves the custom OpenAI configuration as JSON
    ///
    /// # Arguments
    /// * `pool` - Database connection pool
    /// * `config` - CustomOpenAIConfig to save (includes endpoint, apiKey, model, maxTokens, temperature, topP)
    ///
    /// # Returns
    /// * `Ok(())` - Config saved successfully
    /// * `Err(sqlx::Error)` - Database or JSON serialization error
    pub async fn save_custom_openai_config(
        pool: &SqlitePool,
        config: &CustomOpenAIConfig,
    ) -> std::result::Result<(), sqlx::Error> {
        // Serialize config to JSON
        let config_json = serde_json::to_string(config)
            .map_err(|e| sqlx::Error::Protocol(
                format!("Failed to serialize config to JSON: {}", e).into()
            ))?;

        // Upsert into settings table
        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, customOpenAIConfig)
            VALUES ('1', 'custom-openai', $1, 'large-v3', $2)
            ON CONFLICT(id) DO UPDATE SET
                customOpenAIConfig = excluded.customOpenAIConfig
            "#,
        )
        .bind(&config.model)
        .bind(config_json)
        .execute(pool)
        .await?;

        Ok(())
    }

    // ===== CUSTOM VOCABULARY CONFIG METHODS =====

    /// Gets the custom vocabulary configuration from JSON
    ///
    /// # Returns
    /// * `Ok(Some(VocabularyConfig))` - Config exists and is valid JSON
    /// * `Ok(None)` - No config stored
    /// * `Err(sqlx::Error)` - Database error
    pub async fn get_vocabulary_config(
        pool: &SqlitePool,
    ) -> std::result::Result<Option<VocabularyConfig>, sqlx::Error> {
        use sqlx::Row;

        let row = sqlx::query(
            r#"
            SELECT vocabularyConfig
            FROM settings
            WHERE id = '1'
            LIMIT 1
            "#,
        )
        .fetch_optional(pool)
        .await?;

        match row {
            Some(record) => {
                let config_json: Option<String> = record.get("vocabularyConfig");

                if let Some(json) = config_json {
                    let config: VocabularyConfig = serde_json::from_str(&json).map_err(|e| {
                        sqlx::Error::Protocol(
                            format!("Invalid JSON in vocabularyConfig: {}", e).into(),
                        )
                    })?;

                    Ok(Some(config))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    /// Saves the custom vocabulary configuration as JSON
    ///
    /// # Arguments
    /// * `pool` - Database connection pool
    /// * `config` - VocabularyConfig to save
    ///
    /// # Returns
    /// * `Ok(())` - Config saved successfully
    /// * `Err(sqlx::Error)` - Database or JSON serialization error
    pub async fn save_vocabulary_config(
        pool: &SqlitePool,
        config: &VocabularyConfig,
    ) -> std::result::Result<(), sqlx::Error> {
        let config_json = serde_json::to_string(config).map_err(|e| {
            sqlx::Error::Protocol(format!("Failed to serialize vocabularyConfig: {}", e).into())
        })?;

        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, vocabularyConfig)
            VALUES ('1', 'openrouter', '', 'large-v3', $1)
            ON CONFLICT(id) DO UPDATE SET
                vocabularyConfig = excluded.vocabularyConfig
            "#,
        )
        .bind(config_json)
        .execute(pool)
        .await?;

        Ok(())
    }

    // ===== NEOHIVE CONNECTION SETTINGS =====

    /// Gets the NeoHive connection settings (endpoint, enabled flag, auth method
    /// type + method-specific config JSON)
    ///
    /// # Returns
    /// * `Ok(NeoHiveSettings)` - Stored config, or defaults if no row exists yet
    /// * `Err(sqlx::Error)` - Database error
    pub async fn get_neohive_config(
        pool: &SqlitePool,
    ) -> std::result::Result<NeoHiveSettings, sqlx::Error> {
        let row: Option<(Option<String>, Option<i64>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT neohiveEndpoint, neohiveEnabled, neohiveAuthType, neohiveAuthConfig FROM settings WHERE id = '1' LIMIT 1",
        )
        .fetch_optional(pool)
        .await?;
        Ok(match row {
            Some((endpoint, enabled, auth_type, auth_config)) => NeoHiveSettings {
                endpoint,
                enabled: enabled.unwrap_or(0) != 0,
                auth_type,
                auth_config,
            },
            None => NeoHiveSettings::default(),
        })
    }

    /// Saves the NeoHive connection settings, upserting the single settings row (id = '1')
    ///
    /// # Returns
    /// * `Ok(())` - Config saved successfully
    /// * `Err(sqlx::Error)` - Database error
    pub async fn save_neohive_config(
        pool: &SqlitePool,
        endpoint: Option<&str>,
        enabled: bool,
        auth_type: Option<&str>,
        auth_config: Option<&str>,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, neohiveEndpoint, neohiveEnabled, neohiveAuthType, neohiveAuthConfig)
            VALUES ('1', 'openai', 'gpt-4o-2024-11-20', 'large-v3', ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                neohiveEndpoint = excluded.neohiveEndpoint,
                neohiveEnabled = excluded.neohiveEnabled,
                neohiveAuthType = excluded.neohiveAuthType,
                neohiveAuthConfig = excluded.neohiveAuthConfig
            "#,
        )
        .bind(endpoint)
        .bind(if enabled { 1_i64 } else { 0_i64 })
        .bind(auth_type)
        .bind(auth_config)
        .execute(pool)
        .await?;
        Ok(())
    }

    // ===== OBSIDIAN VAULT SETTINGS =====

    pub async fn get_obsidian_config(
        pool: &SqlitePool,
    ) -> std::result::Result<ObsidianSettings, sqlx::Error> {
        let row: Option<(Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT obsidianVaultPath, obsidianEnabled FROM settings WHERE id = '1' LIMIT 1",
        )
        .fetch_optional(pool)
        .await?;
        Ok(match row {
            Some((vault_path, enabled)) => ObsidianSettings {
                vault_path,
                enabled: enabled.unwrap_or(0) != 0,
            },
            None => ObsidianSettings::default(),
        })
    }

    pub async fn save_obsidian_config(
        pool: &SqlitePool,
        vault_path: Option<&str>,
        enabled: bool,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, obsidianVaultPath, obsidianEnabled)
            VALUES ('1', 'openai', 'gpt-4o-2024-11-20', 'large-v3', ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                obsidianVaultPath = excluded.obsidianVaultPath,
                obsidianEnabled = excluded.obsidianEnabled
            "#,
        )
        .bind(vault_path)
        .bind(if enabled { 1_i64 } else { 0_i64 })
        .execute(pool)
        .await?;
        Ok(())
    }


    // ===== GOOGLE CALENDAR CONFIG METHODS =====

    pub async fn get_google_calendar_config(
        pool: &SqlitePool,
    ) -> std::result::Result<Option<crate::calendar::GoogleCalendarConfig>, sqlx::Error> {
        use sqlx::Row;

        let row = sqlx::query("SELECT googleCalendarConfig FROM settings WHERE id = '1' LIMIT 1")
            .fetch_optional(pool)
            .await?;

        match row {
            Some(record) => {
                let config_json: Option<String> = record.get("googleCalendarConfig");
                if let Some(json) = config_json {
                    let config: crate::calendar::GoogleCalendarConfig =
                        serde_json::from_str(&json).map_err(|e| {
                            sqlx::Error::Protocol(
                                format!("Invalid JSON in googleCalendarConfig: {}", e).into(),
                            )
                        })?;
                    Ok(Some(config))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    pub async fn save_google_calendar_config(
        pool: &SqlitePool,
        config: &crate::calendar::GoogleCalendarConfig,
    ) -> std::result::Result<(), sqlx::Error> {
        let config_json = serde_json::to_string(config).map_err(|e| {
            sqlx::Error::Protocol(format!("Failed to serialize calendar config: {}", e).into())
        })?;

        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, googleCalendarConfig)
            VALUES ('1', 'openai', 'gpt-4o-2024-11-20', 'large-v3', $1)
            ON CONFLICT(id) DO UPDATE SET
                googleCalendarConfig = excluded.googleCalendarConfig
            "#,
        )
        .bind(config_json)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn clear_google_calendar_config(
        pool: &SqlitePool,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query("UPDATE settings SET googleCalendarConfig = NULL WHERE id = '1'")
            .execute(pool)
            .await?;
        Ok(())
    }

    pub async fn get_auto_detect_meet_enabled(
        pool: &SqlitePool,
    ) -> std::result::Result<bool, sqlx::Error> {
        let value: Option<i64> =
            sqlx::query_scalar("SELECT autoDetectMeetEnabled FROM settings WHERE id = '1' LIMIT 1")
                .fetch_optional(pool)
                .await?;
        Ok(value.unwrap_or(0) != 0)
    }

    pub async fn set_auto_detect_meet_enabled(
        pool: &SqlitePool,
        enabled: bool,
    ) -> std::result::Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, autoDetectMeetEnabled)
            VALUES ('1', 'openai', 'gpt-4o-2024-11-20', 'large-v3', $1)
            ON CONFLICT(id) DO UPDATE SET
                autoDetectMeetEnabled = excluded.autoDetectMeetEnabled
            "#,
        )
        .bind(enabled as i64)
        .execute(pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod neohive_settings_tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new().max_connections(1)
            .connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn save_then_get_neohive_config_bearer() {
        let pool = test_pool().await;
        SettingsRepository::save_neohive_config(
            &pool,
            Some("https://neo.example/mcp"),
            true,
            Some("bearer"),
            Some(r#"{"token":"tok-123"}"#),
        ).await.unwrap();
        let cfg = SettingsRepository::get_neohive_config(&pool).await.unwrap();
        assert_eq!(cfg.endpoint.as_deref(), Some("https://neo.example/mcp"));
        assert!(cfg.enabled);
        assert_eq!(cfg.auth_type.as_deref(), Some("bearer"));
        assert_eq!(cfg.auth_config.as_deref(), Some(r#"{"token":"tok-123"}"#));
    }

    #[tokio::test]
    async fn get_neohive_config_defaults_when_unset() {
        let pool = test_pool().await;
        let cfg = SettingsRepository::get_neohive_config(&pool).await.unwrap();
        assert!(cfg.endpoint.is_none());
        assert!(cfg.auth_type.is_none());
        assert!(cfg.auth_config.is_none());
        assert!(!cfg.enabled);
    }

    /// Proves the `20260709000002_add_neohive_auth_method.sql` backfill UPDATE
    /// correctly migrates a pre-existing Cloudflare Access config (the old
    /// dedicated columns) into the new generic neohiveAuthType/neohiveAuthConfig
    /// columns, and that the resulting shape actually builds working Cloudflare
    /// auth via `NeoHiveAuth::from_parts`.
    #[tokio::test]
    async fn backfill_transforms_legacy_cloudflare_config() {
        let pool = test_pool().await;

        // Simulate a row written before the neohiveAuthType/neohiveAuthConfig
        // columns existed: only the legacy Cloudflare-specific columns are set.
        sqlx::query(
            r#"
            INSERT INTO settings (id, provider, model, whisperModel, neohiveAccessClientId, neohiveAccessClientSecret)
            VALUES ('1', 'openai', 'gpt-4o-2024-11-20', 'large-v3', 'cid-legacy', 'csec-legacy')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        // Run the exact backfill transform from the migration.
        sqlx::query(
            r#"
            UPDATE settings
            SET neohiveAuthType = 'cloudflare_access',
                neohiveAuthConfig = json_object('clientId', neohiveAccessClientId, 'clientSecret', neohiveAccessClientSecret)
            WHERE neohiveAccessClientId IS NOT NULL OR neohiveAccessClientSecret IS NOT NULL
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let cfg = SettingsRepository::get_neohive_config(&pool).await.unwrap();
        assert_eq!(cfg.auth_type.as_deref(), Some("cloudflare_access"));

        let auth_config_json = cfg.auth_config.expect("backfill should have set neohiveAuthConfig");
        let parsed: serde_json::Value = serde_json::from_str(&auth_config_json).unwrap();
        assert_eq!(parsed.get("clientId").and_then(|v| v.as_str()), Some("cid-legacy"));
        assert_eq!(parsed.get("clientSecret").and_then(|v| v.as_str()), Some("csec-legacy"));

        // Strengthen: the backfilled shape must actually produce working Cloudflare auth.
        let auth = crate::neohive::NeoHiveAuth::from_parts(cfg.auth_type.as_deref(), &parsed).unwrap();
        assert!(matches!(
            &auth,
            crate::neohive::NeoHiveAuth::CloudflareAccess { client_id, client_secret }
                if client_id == "cid-legacy" && client_secret == "csec-legacy"
        ));
    }
}

#[cfg(test)]
mod obsidian_settings_tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn defaults_then_save_then_read() {
        let pool = pool().await;
        let cfg = SettingsRepository::get_obsidian_config(&pool).await.unwrap();
        assert!(cfg.vault_path.is_none());
        assert!(!cfg.enabled);

        SettingsRepository::save_obsidian_config(&pool, Some("/vault/Meetings"), true).await.unwrap();
        let cfg = SettingsRepository::get_obsidian_config(&pool).await.unwrap();
        assert_eq!(cfg.vault_path.as_deref(), Some("/vault/Meetings"));
        assert!(cfg.enabled);
    }
}
