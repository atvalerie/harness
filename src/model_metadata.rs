//! Codex metadata is a labelled fallback, never an inferred gateway guarantee.
use crate::{
    client::{types::ModelInfo, AiClient, ProviderKind, ProviderProtocol},
    config::AppConfig,
};
use std::time::Duration;
#[derive(serde::Serialize, serde::Deserialize)]
struct CatalogCache {
    fetched_at: chrono::DateTime<chrono::Utc>,
    models: Vec<ModelInfo>,
}
pub async fn enrich(config: &AppConfig, models: &mut [ModelInfo]) {
    let cache_path = AppConfig::config_dir().map(|dir| dir.join("codex-models.json"));
    if config
        .active_provider_config()
        .kind
        .eq_ignore_ascii_case("codex")
    {
        if let Some(path) = cache_path {
            if let Ok(data) = serde_json::to_vec(&CatalogCache {
                fetched_at: chrono::Utc::now(),
                models: models.to_vec(),
            }) {
                let _ = std::fs::write(path, data);
            }
        }
        return;
    }
    let bearlab = config.provider.to_lowercase().contains("bearlab");
    let mapped = models.iter().any(|m| {
        config
            .model_profiles
            .get(&format!("{}:{}", config.provider, m.id))
            .is_some_and(|p| p.codex_model.is_some())
    });
    if !bearlab && !mapped {
        return;
    }
    let cached = cache_path
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<CatalogCache>(&b).ok());
    let mut catalog = cached
        .as_ref()
        .map(|c| c.models.clone())
        .unwrap_or_default();
    let mut source = cached
        .as_ref()
        .map(|c| {
            format!(
                "Codex catalog fallback (cached {})",
                c.fetched_at.format("%Y-%m-%d")
            )
        })
        .unwrap_or_default();
    if cached.as_ref().is_none_or(|c| {
        chrono::Utc::now()
            .signed_duration_since(c.fetched_at)
            .num_hours()
            >= 24
    }) {
        // NEVER send a BearLab key to Codex. Use only independently stored Codex OAuth credentials.
        if let Some(auth) = AppConfig::get_codex_auth() {
            let client = AiClient::with_provider(
                auth.access_token,
                ProviderKind::Codex,
                None,
                Default::default(),
                true,
                ProviderProtocol::Responses,
                auth.account_id,
            );
            if let Ok(Ok(fresh)) =
                tokio::time::timeout(Duration::from_secs(5), client.list_models()).await
            {
                source = "Codex catalog fallback (live)".into();
                catalog = fresh;
                if let Some(path) = cache_path {
                    if let Ok(data) = serde_json::to_vec(&CatalogCache {
                        fetched_at: chrono::Utc::now(),
                        models: catalog.clone(),
                    }) {
                        let _ = std::fs::write(path, data);
                    }
                }
            }
        }
    }
    for model in models {
        let explicit = config
            .model_profiles
            .get(&format!("{}:{}", config.provider, model.id))
            .and_then(|p| p.codex_model.as_deref());
        let id = explicit.unwrap_or(&model.id);
        if let Some(upstream) = catalog.iter().find(|c| c.id == id) {
            apply_fallback(model, upstream, &source);
        }
    }
}
fn apply_fallback(model: &mut ModelInfo, upstream: &ModelInfo, source: &str) {
    let mut used = false;
    if model.context_window.is_none() && model.input_token_limit.is_none() {
        model.context_window = upstream.context_window;
        model.input_token_limit = upstream.input_token_limit;
        used = model.context_window.is_some() || model.input_token_limit.is_some();
    }
    if model.output_token_limit.is_none() {
        model.output_token_limit = upstream.output_token_limit;
        used |= model.output_token_limit.is_some();
    }
    if model.reasoning_levels.is_empty() {
        model.reasoning_levels = upstream.reasoning_levels.clone();
        used |= !model.reasoning_levels.is_empty();
    }
    if used {
        model.metadata_source = format!("provider catalog + {source} [{}]", upstream.id);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gateway_limits_take_precedence() {
        let mut model:ModelInfo=serde_json::from_value(serde_json::json!({"id":"gateway","display_name":"test","description":"","context_window":64000})).unwrap();
        let mut upstream = model.clone();
        upstream.context_window = Some(128000);
        upstream.reasoning_levels = vec!["high".into()];
        apply_fallback(&mut model, &upstream, "test");
        assert_eq!(model.context_window, Some(64000));
        assert_eq!(model.reasoning_levels, vec!["high"]);
    }
}
