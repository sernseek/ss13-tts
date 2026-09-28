use std::time::Duration;

use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{config::Config, error::AppError, voices::Voice};

const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
const SAMPLE_RATE: u32 = 24_000;

/// Control tags: they set the emotion or delivery of all text that follows them.
pub const STYLE_TAGS: &[&str] = &[
    "sad",
    "amazed",
    "deep and loud shouting",
    "trembling",
    "angry",
    "excited",
    "sarcastic",
    "curious",
    "like dracula",
    "bored",
    "tired",
    "scornful",
    "shouting",
    "asmr",
    "panicked",
    "mischievously",
    "empathetic",
    "whispers",
    "reluctantly",
    "crying",
    "serious",
    "very slowly",
    "very fast",
];

/// Paralinguistic tags: they insert a nonverbal sound where they appear.
pub const SOUND_TAGS: &[&str] = &[
    "gasp",
    "sighing",
    "clears throat",
    "giggles",
    "laughing",
    "cough",
    "snorts",
];

/// Which DashScope API family a model belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// qwen-audio-* and cosyvoice-*: the audio/tts/SpeechSynthesizer API. Supports inline
    /// emotion tags, natural-language instructions, rate and pitch.
    QwenAudio,
    /// qwen3-tts-* and qwen-tts-*: the multimodal-generation API. Text and voice only.
    QwenTts,
}

impl Backend {
    pub fn for_model(model: &str) -> Self {
        if model.starts_with("qwen-audio-") || model.starts_with("cosyvoice-") {
            Self::QwenAudio
        } else {
            Self::QwenTts
        }
    }

    /// The API family a DashScope endpoint belongs to, if it is recognizable.
    pub fn for_endpoint(endpoint: &str) -> Option<Self> {
        let path = endpoint.trim_end_matches('/');
        if path.ends_with("/services/audio/tts/SpeechSynthesizer") {
            Some(Self::QwenAudio)
        } else if path.ends_with("/services/aigc/multimodal-generation/generation") {
            Some(Self::QwenTts)
        } else {
            None
        }
    }

    pub fn default_endpoint(self) -> &'static str {
        match self {
            Self::QwenAudio => {
                "https://dashscope.aliyuncs.com/api/v1/services/audio/tts/SpeechSynthesizer"
            }
            Self::QwenTts => {
                "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation"
            }
        }
    }

    pub fn supports_expression(self) -> bool {
        self == Self::QwenAudio
    }
}

/// Everything that shapes one synthesized clip.
#[derive(Clone, Copy, Debug)]
pub struct Synthesis<'a> {
    pub text: &'a str,
    pub voice: &'a Voice,
    pub style: Option<&'static str>,
    pub sound: Option<&'static str>,
    pub instruction: Option<&'a str>,
    pub rate: Option<f32>,
    pub pitch: Option<f32>,
}

#[derive(Clone)]
pub struct DashscopeClient {
    client: Client,
    endpoint: String,
    api_key: String,
    model: String,
    backend: Backend,
    language_type: String,
    language_hint: Option<String>,
    hot_fix: Option<Value>,
    allow_untrusted_audio_urls: bool,
}

impl DashscopeClient {
    pub fn new(config: &Config) -> Result<Self, AppError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(config.provider_timeout)
            .build()
            .map_err(|error| AppError::Internal(format!("failed to build HTTP client: {error}")))?;
        Ok(Self {
            client,
            endpoint: config.dashscope_endpoint.clone(),
            api_key: config.dashscope_api_key.clone(),
            model: config.model.clone(),
            backend: config.backend,
            language_type: config.language_type.clone(),
            language_hint: config.language_hint.clone(),
            hot_fix: config.hot_fix.clone(),
            allow_untrusted_audio_urls: config.allow_untrusted_audio_urls,
        })
    }

    /// Builds the provider request body. It is also the cache identity of the clip, so
    /// everything that changes the audio must be part of it.
    pub fn request_body(&self, synthesis: &Synthesis<'_>) -> Value {
        let text = escape_tags(synthesis.text);
        match self.backend {
            Backend::QwenTts => json!({
                "model": self.model,
                "input": {
                    "text": text,
                    "voice": synthesis.voice.api,
                    "language_type": self.language_type,
                },
            }),
            Backend::QwenAudio => {
                let mut tagged = String::with_capacity(text.len() + 32);
                if let Some(sound) = synthesis.sound {
                    tagged.push_str(&format!("[{sound}]"));
                }
                if let Some(style) = synthesis.style {
                    tagged.push_str(&format!("[{style}]"));
                }
                tagged.push_str(&text);

                let mut input = json!({
                    "text": tagged,
                    "voice": synthesis.voice.api,
                    "format": "wav",
                    "sample_rate": SAMPLE_RATE,
                });
                if let Some(instruction) = synthesis.instruction {
                    input["instruction"] = json!(escape_tags(instruction));
                }
                if let Some(rate) = synthesis.rate {
                    input["rate"] = json!(rate);
                }
                if let Some(pitch) = synthesis.pitch {
                    input["pitch"] = json!(pitch);
                }
                if let Some(language) = synthesis.voice.language.or(self.language_hint.as_deref()) {
                    input["language_hints"] = json!([language]);
                }
                if synthesis.voice.aigc {
                    input["enable_aigc_tag"] = json!(true);
                }
                if let Some(hot_fix) = &self.hot_fix {
                    input["hot_fix"] = hot_fix.clone();
                }
                json!({ "model": self.model, "input": input })
            }
        }
    }

    pub async fn synthesize(&self, request: &Value) -> Result<Vec<u8>, AppError> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(request)
            .send()
            .await
            .map_err(|error| AppError::Provider(format!("DashScope request failed: {error}")))?;
        let status = response.status();
        let body = response.bytes().await.map_err(|error| {
            AppError::Provider(format!("failed to read DashScope response: {error}"))
        })?;
        let payload: DashscopeResponse = serde_json::from_slice(&body).map_err(|error| {
            AppError::Provider(format!(
                "DashScope returned invalid JSON with HTTP {status}: {error}"
            ))
        })?;
        if !status.is_success() || payload.output.is_none() {
            return Err(provider_error(status, &payload));
        }

        let audio = payload
            .output
            .and_then(|output| output.audio)
            .ok_or_else(|| AppError::Provider("DashScope response contained no audio".into()))?;
        let audio_url = audio.url.filter(|url| !url.is_empty()).ok_or_else(|| {
            AppError::Provider("DashScope response contained no audio URL".into())
        })?;
        self.validate_audio_url(&audio_url)?;

        let usage = payload.usage.unwrap_or_default();
        info!(
            request_id = payload.request_id.as_deref().unwrap_or("unknown"),
            billed_characters = usage.characters,
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            "DashScope synthesis completed"
        );

        let response = self
            .client
            .get(audio_url)
            .send()
            .await
            .map_err(|error| AppError::Provider(format!("audio download failed: {error}")))?;
        if response.status() != StatusCode::OK {
            return Err(AppError::Provider(format!(
                "audio download returned HTTP {}",
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_AUDIO_BYTES as u64)
        {
            return Err(AppError::Provider(
                "audio response exceeds size limit".into(),
            ));
        }
        let audio = response.bytes().await.map_err(|error| {
            AppError::Provider(format!("failed to read audio response: {error}"))
        })?;
        if audio.len() > MAX_AUDIO_BYTES {
            return Err(AppError::Provider(
                "audio response exceeds size limit".into(),
            ));
        }
        Ok(audio.to_vec())
    }

    fn validate_audio_url(&self, value: &str) -> Result<(), AppError> {
        let url = Url::parse(value)
            .map_err(|error| AppError::Provider(format!("invalid audio URL: {error}")))?;
        if self.allow_untrusted_audio_urls {
            warn!(
                host = url.host_str().unwrap_or("none"),
                "untrusted audio URLs are enabled"
            );
            return Ok(());
        }
        let host = url
            .host_str()
            .ok_or_else(|| AppError::Provider("audio URL has no host".into()))?;
        if !matches!(url.scheme(), "http" | "https")
            || !(host == "aliyuncs.com" || host.ends_with(".aliyuncs.com"))
        {
            return Err(AppError::Provider(
                "DashScope returned an untrusted audio URL".into(),
            ));
        }
        Ok(())
    }
}

/// Player text must never reach the model as a control tag: only the allowlisted `style`
/// and `sound` fields may add tags. Square brackets become parentheses, which the model
/// reads as ordinary punctuation.
fn escape_tags(text: &str) -> String {
    text.replace('[', "(").replace(']', ")")
}

#[derive(Deserialize)]
struct DashscopeResponse {
    request_id: Option<String>,
    code: Option<String>,
    message: Option<String>,
    output: Option<DashscopeOutput>,
    usage: Option<DashscopeUsage>,
}

#[derive(Deserialize)]
struct DashscopeOutput {
    audio: Option<DashscopeAudio>,
}

#[derive(Deserialize)]
struct DashscopeAudio {
    url: Option<String>,
}

#[derive(Default, Deserialize)]
struct DashscopeUsage {
    characters: Option<u64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

fn provider_error(status: StatusCode, payload: &DashscopeResponse) -> AppError {
    let code = payload.code.as_deref().unwrap_or("unknown_error");
    let message = payload.message.as_deref().unwrap_or("no error message");
    AppError::Provider(format!("DashScope HTTP {status} ({code}): {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_brackets_cannot_inject_tags() {
        assert_eq!(escape_tags("[laughing]嗨[x]"), "(laughing)嗨(x)");
    }

    #[test]
    fn detects_backend_from_model() {
        assert_eq!(
            Backend::for_model("qwen-audio-3.1-tts-flash"),
            Backend::QwenAudio
        );
        assert_eq!(Backend::for_model("cosyvoice-v3-flash"), Backend::QwenAudio);
        assert_eq!(
            Backend::for_model("qwen3-tts-flash-2025-11-27"),
            Backend::QwenTts
        );
    }
}
