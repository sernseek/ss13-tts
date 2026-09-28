use std::{env, net::SocketAddr, path::PathBuf, time::Duration};

use serde_json::Value;

use crate::provider::Backend;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub authorization_token: String,
    pub dashscope_api_key: String,
    pub dashscope_endpoint: String,
    pub model: String,
    pub backend: Backend,
    pub language_type: String,
    /// Default `language_hints` entry for qwen-audio models, e.g. `zh`.
    pub language_hint: Option<String>,
    /// JSON object passed to qwen-audio models as `hot_fix` (pronunciation and replacements).
    pub hot_fix: Option<Value>,
    /// JSON file listing cloned or designed voices.
    pub custom_voices: Option<PathBuf>,
    /// Where the adapter keeps player-made voices.
    pub data_dir: PathBuf,
    /// The voice-enrollment API (voice design and cloning).
    pub customization_endpoint: String,
    /// DashScope temporary file storage, used to hand recordings to voice cloning.
    pub uploads_endpoint: String,
    /// Active player-made voices allowed per player.
    pub custom_voice_limit: usize,
    /// Voices (designed or cloned) a player may create per day.
    pub custom_voice_daily_creations: usize,
    pub provider_timeout: Duration,
    pub provider_concurrency: usize,
    pub cache_dir: PathBuf,
    pub cache_max_bytes: u64,
    pub cache_ttl: Duration,
    pub max_text_chars: usize,
    pub allow_untrusted_audio_urls: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        dotenvy::dotenv().ok();

        let authorization_token = required("TTS_AUTHORIZATION_TOKEN")?;
        let dashscope_api_key = required("DASHSCOPE_API_KEY")?;
        let bind_addr = value("TTS_BIND_ADDR", "127.0.0.1:5002")
            .parse()
            .map_err(|error| format!("invalid TTS_BIND_ADDR: {error}"))?;
        let provider_timeout = Duration::from_millis(parse("DASHSCOPE_TIMEOUT_MS", 5_500_u64)?);
        let provider_concurrency = parse("DASHSCOPE_MAX_CONCURRENCY", 3_usize)?;
        if provider_concurrency == 0 {
            return Err("DASHSCOPE_MAX_CONCURRENCY must be greater than zero".into());
        }

        let model = value("QWEN_TTS_MODEL", "qwen-audio-3.1-tts-flash");
        let backend = Backend::for_model(&model);
        let hot_fix = optional("QWEN_TTS_HOT_FIX_FILE")
            .map(|path| load_hot_fix(&PathBuf::from(path)))
            .transpose()?;

        let dashscope_endpoint = value("DASHSCOPE_ENDPOINT", backend.default_endpoint());
        let endpoint_family = Backend::for_endpoint(&dashscope_endpoint);
        if endpoint_family.is_some_and(|family| family != backend) {
            return Err(format!(
                "DASHSCOPE_ENDPOINT {dashscope_endpoint} does not serve {model}; remove it to use {}",
                backend.default_endpoint()
            ));
        }

        Ok(Self {
            bind_addr,
            authorization_token,
            dashscope_api_key,
            dashscope_endpoint: dashscope_endpoint.clone(),
            model,
            backend,
            language_type: value("QWEN_TTS_LANGUAGE_TYPE", "Auto"),
            language_hint: optional("QWEN_TTS_LANGUAGE_HINT"),
            hot_fix,
            custom_voices: optional("QWEN_TTS_CUSTOM_VOICES").map(PathBuf::from),
            data_dir: PathBuf::from(value("TTS_DATA_DIR", "./data")),
            customization_endpoint: value(
                "DASHSCOPE_CUSTOMIZATION_ENDPOINT",
                &sibling_endpoint(&dashscope_endpoint, "services/audio/tts/customization"),
            ),
            uploads_endpoint: value(
                "DASHSCOPE_UPLOADS_ENDPOINT",
                &sibling_endpoint(&dashscope_endpoint, "uploads"),
            ),
            custom_voice_limit: parse("TTS_CUSTOM_VOICE_LIMIT", 3_usize)?,
            custom_voice_daily_creations: parse("TTS_CUSTOM_VOICE_DAILY_CREATIONS", 10_usize)?,
            provider_timeout,
            provider_concurrency,
            cache_dir: PathBuf::from(value("TTS_CACHE_DIR", "./cache")),
            cache_max_bytes: parse("TTS_CACHE_MAX_BYTES", 5_u64 * 1024 * 1024 * 1024)?,
            cache_ttl: Duration::from_secs(parse("TTS_CACHE_TTL_SECONDS", 86_400_u64)?),
            max_text_chars: parse("TTS_MAX_TEXT_CHARS", 1_200_usize)?,
            allow_untrusted_audio_urls: parse_bool("QWEN_TTS_ALLOW_UNTRUSTED_AUDIO_URLS", false)?,
        })
    }
}

fn required(name: &str) -> Result<String, String> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} is required"))
}

/// Another DashScope API on the same host and version as `endpoint`.
fn sibling_endpoint(endpoint: &str, path: &str) -> String {
    match endpoint.find("/api/v1/") {
        Some(index) => format!("{}/api/v1/{path}", &endpoint[..index]),
        None => format!("https://dashscope.aliyuncs.com/api/v1/{path}"),
    }
}

fn optional(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn load_hot_fix(path: &PathBuf) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "cannot read QWEN_TTS_HOT_FIX_FILE {}: {error}",
            path.display()
        )
    })?;
    let hot_fix: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid QWEN_TTS_HOT_FIX_FILE {}: {error}", path.display()))?;
    if !hot_fix.is_object() {
        return Err("QWEN_TTS_HOT_FIX_FILE must contain a JSON object".into());
    }
    Ok(hot_fix)
}

fn value(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn parse<T>(name: &str, default: T) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|error| format!("invalid {name}: {error}")),
        Err(_) => Ok(default),
    }
}

fn parse_bool(name: &str, default: bool) -> Result<bool, String> {
    match env::var(name) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Ok(true),
            "0" | "false" | "no" => Ok(false),
            _ => Err(format!("invalid {name}: expected true or false")),
        },
        Err(_) => Ok(default),
    }
}
