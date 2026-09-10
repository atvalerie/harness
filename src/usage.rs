//! Provider-reported usage and a bounded, append-only accounting sink.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}
impl TokenUsage {
    pub fn openai(value: &Value, responses: bool) -> Option<Self> {
        let (input, output, details, out_details) = if responses {
            (
                "input_tokens",
                "output_tokens",
                "input_tokens_details",
                "output_tokens_details",
            )
        } else {
            (
                "prompt_tokens",
                "completion_tokens",
                "prompt_tokens_details",
                "completion_tokens_details",
            )
        };
        let usage = Self {
            input_tokens: value.get(input).and_then(Value::as_u64),
            output_tokens: value.get(output).and_then(Value::as_u64),
            total_tokens: value.get("total_tokens").and_then(Value::as_u64),
            cache_read_tokens: value
                .get(details)
                .and_then(|v| v.get("cached_tokens"))
                .and_then(Value::as_u64),
            reasoning_tokens: value
                .get(out_details)
                .and_then(|v| v.get("reasoning_tokens"))
                .and_then(Value::as_u64),
            ..Self::default()
        };
        usage.has_counts().then_some(usage)
    }
    pub fn has_counts(&self) -> bool {
        self.input_tokens.is_some() || self.output_tokens.is_some() || self.total_tokens.is_some()
    }
    pub fn complete(&self) -> bool {
        self.input_tokens.is_some() && self.output_tokens.is_some()
    }
    pub fn total(&self) -> Option<u64> {
        self.total_tokens
            .or_else(|| Some(self.input_tokens?.saturating_add(self.output_tokens?)))
    }
    // Usage events are snapshots. Preserve omitted fields without adding repeated totals.
    pub fn merge(&mut self, next: &Self) {
        macro_rules! field {
            ($f:ident) => {
                if next.$f.is_some() {
                    self.$f = next.$f;
                }
            };
        }
        field!(input_tokens);
        field!(output_tokens);
        field!(total_tokens);
        field!(cache_read_tokens);
        field!(cache_write_tokens);
        field!(reasoning_tokens);
    }
    pub fn signal(self) -> crate::events::StreamSignal {
        crate::events::StreamSignal::Usage {
            prompt_tokens: self.input_tokens.unwrap_or(0),
            candidates_tokens: self.output_tokens.unwrap_or(0),
            total_tokens: self.total().unwrap_or(0),
            details: self,
        }
    }
}

fn endpoint_identity(endpoint: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(endpoint) else {
        return "unknown".into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub requests: u64,
    pub reported: u64,
    pub input: u64,
    pub output: u64,
    pub cache_known: u64,
    pub cache_input: u64,
    pub cache_read: u64,
}
impl Summary {
    pub fn add(&mut self, usage: &TokenUsage) {
        self.requests += 1;
        if usage.complete() {
            self.reported += 1;
        }
        self.input = self.input.saturating_add(usage.input_tokens.unwrap_or(0));
        self.output = self.output.saturating_add(usage.output_tokens.unwrap_or(0));
        if let (Some(input), Some(cached)) = (usage.input_tokens, usage.cache_read_tokens) {
            if cached <= input {
                self.cache_known += 1;
                self.cache_input = self.cache_input.saturating_add(input);
                self.cache_read = self.cache_read.saturating_add(cached);
            }
        }
    }
    pub fn cache_rate(&self) -> String {
        if self.cache_input == 0 {
            "N/A".into()
        } else {
            format!(
                "{:.1}%",
                self.cache_read as f64 * 100.0 / self.cache_input as f64
            )
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pricing {
    pub version: String,
    pub input_micro_usd_per_m: u64,
    pub output_micro_usd_per_m: u64,
    pub cached_input_micro_usd_per_m: Option<u64>,
}
impl Pricing {
    /// Returns nanodollars, rounded once per request. Flat text-token pricing only.
    pub fn cost_nano_usd(&self, usage: &TokenUsage) -> Option<u64> {
        let input = usage.input_tokens?;
        let output = usage.output_tokens?;
        if usage.cache_write_tokens.unwrap_or(0) > 0 {
            return None;
        }
        let cached = if self.cached_input_micro_usd_per_m.is_some() {
            usage.cache_read_tokens?
        } else {
            0
        };
        let fresh = input.checked_sub(cached)?;
        let numerator = u128::from(fresh) * u128::from(self.input_micro_usd_per_m)
            + u128::from(cached)
                * u128::from(
                    self.cached_input_micro_usd_per_m
                        .unwrap_or(self.input_micro_usd_per_m),
                )
            + u128::from(output) * u128::from(self.output_micro_usd_per_m);
        u64::try_from((numerator + 500) / 1000).ok()
    }
}

#[derive(Serialize)]
struct Record {
    version: u32,
    attempt_id: String,
    timestamp: String,
    provider: String,
    model: String,
    mode: &'static str,
    status: String,
    duration_ms: u64,
    usage: TokenUsage,
    pricing: Option<Pricing>,
    cost_nano_usd: Option<u64>,
}

/// One small record per HTTP attempt, including dropped/cancelled futures.
/// No transcript retention, per-token writes, database, or per-request fsync.
pub struct Attempt {
    id: String,
    provider: String,
    model: String,
    mode: &'static str,
    started: std::time::Instant,
    pub status: String,
    pub usage: TokenUsage,
    pub pricing: Option<Pricing>,
}
impl Attempt {
    pub fn new(provider: &str, model: &str, mode: &'static str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self {
            id: format!(
                "{}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
            provider: endpoint_identity(provider),
            model: model.into(),
            mode,
            started: std::time::Instant::now(),
            status: "interrupted".into(),
            usage: TokenUsage::default(),
            pricing: None,
        }
    }
}
static PROCESS_SUMMARY: Mutex<Summary> = Mutex::new(Summary {
    requests: 0,
    reported: 0,
    input: 0,
    output: 0,
    cache_known: 0,
    cache_input: 0,
    cache_read: 0,
});
pub fn process_summary() -> Summary {
    PROCESS_SUMMARY
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if let Ok(mut summary) = PROCESS_SUMMARY.lock() {
            summary.add(&self.usage);
        }
        let record = Record {
            version: 1,
            attempt_id: self.id.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            mode: self.mode,
            status: self.status.clone(),
            duration_ms: self.started.elapsed().as_millis() as u64,
            usage: self.usage.clone(),
            pricing: self.pricing.clone(),
            cost_nano_usd: self
                .pricing
                .as_ref()
                .and_then(|p| p.cost_nano_usd(&self.usage)),
        };
        if let Err(error) = append(&record) {
            eprintln!("Cannot append usage journal: {error}");
        }
    }
}
fn append(record: &Record) -> std::io::Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    static LOCK: Mutex<()> = Mutex::new(());
    let _lock = LOCK
        .lock()
        .map_err(|_| std::io::Error::other("usage writer poisoned"))?;
    let dir = crate::config::AppConfig::config_dir()
        .ok_or_else(|| std::io::Error::other("config directory unavailable"))?
        .join("usage");
    std::fs::create_dir_all(&dir)?;
    // Process-specific daily files avoid cross-process interleaving without file locks.
    let path = dir.join(format!(
        "{}-{}.jsonl",
        &record.timestamp[..10],
        std::process::id()
    ));
    append_to(&path, record)
}
fn append_to(path: &std::path::Path, record: &Record) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let mut bytes = serde_json::to_vec(record)?;
    bytes.insert(0, b'\n'); // Isolate a torn prior record.
    bytes.push(b'\n');
    // Direct request-boundary write: no queued records to lose at normal shutdown.
    // Deliberately no fsync; a machine crash can lose recent OS-buffered writes.
    file.write_all(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pricing_and_weighted_cache_coverage() {
        let pricing = Pricing {
            version: "fixture".into(),
            input_micro_usd_per_m: 2_000_000,
            output_micro_usd_per_m: 8_000_000,
            cached_input_micro_usd_per_m: Some(500_000),
        };
        let usage = TokenUsage {
            input_tokens: Some(1000),
            output_tokens: Some(100),
            cache_read_tokens: Some(800),
            ..Default::default()
        };
        assert_eq!(pricing.cost_nano_usd(&usage), Some(1_600_000));
        assert_eq!(
            pricing.cost_nano_usd(&TokenUsage {
                cache_read_tokens: None,
                ..usage.clone()
            }),
            None
        );
        assert_eq!(
            pricing.cost_nano_usd(&TokenUsage {
                cache_read_tokens: Some(1001),
                ..usage.clone()
            }),
            None
        );
        let mut summary = Summary::default();
        summary.add(&usage);
        summary.add(&TokenUsage::default());
        summary.add(&TokenUsage {
            input_tokens: Some(9000),
            cache_read_tokens: Some(0),
            ..Default::default()
        });
        assert_eq!(summary.cache_rate(), "8.0%");
        assert_eq!(summary.cache_known, 2);
        assert_eq!(summary.requests, 3);
    }
    #[test]
    fn journal_appends_after_torn_record() {
        let path = std::env::temp_dir().join(format!(
            "holiday-usage-{}-{}.jsonl",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::write(&path, b"{\"torn\":").unwrap();
        let record = Record {
            version: 1,
            attempt_id: "fixture".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            provider: "test".into(),
            model: "test".into(),
            mode: "stream",
            status: "completed".into(),
            duration_ms: 1,
            usage: TokenUsage::default(),
            pricing: None,
            cost_nano_usd: None,
        };
        append_to(&path, &record).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let valid: Vec<Value> = text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0]["attempt_id"], "fixture");
        assert!(valid[0]["usage"]["input_tokens"].is_null());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn endpoint_credentials_are_not_persisted() {
        assert_eq!(
            endpoint_identity("https://user:secret@example.com/v1?key=secret#secret"),
            "https://example.com/v1"
        );
    }
    #[test]
    fn gemini_reasoning_is_included_once() {
        let metadata: crate::client::types::UsageMetadata = serde_json::from_value(serde_json::json!({"promptTokenCount":100,"candidatesTokenCount":20,"thoughtsTokenCount":10,"cachedContentTokenCount":80,"totalTokenCount":130})).unwrap();
        let usage = metadata.normalized();
        assert_eq!(usage.output_tokens, Some(30));
        assert_eq!(usage.total(), Some(130));
        assert_eq!(usage.cache_read_tokens, Some(80));
    }
    #[test]
    fn missing_is_not_zero_and_snapshots_are_not_added() {
        assert!(TokenUsage::openai(&serde_json::json!(null), false).is_none());
        assert!(TokenUsage::openai(&serde_json::json!({}), true).is_none());
        let mut usage = TokenUsage::openai(&serde_json::json!({"input_tokens":100,"output_tokens":0,"input_tokens_details":{"cached_tokens":80}}), true).unwrap();
        assert!(usage.complete());
        usage.merge(&TokenUsage::openai(&serde_json::json!({"output_tokens":20}), true).unwrap());
        assert_eq!(usage.total(), Some(120));
        assert_eq!(usage.cache_read_tokens, Some(80));
        usage.merge(&usage.clone());
        assert_eq!(usage.total(), Some(120));
    }
}
