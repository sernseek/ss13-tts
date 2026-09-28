//! Player-made voices: voice design (from a description) and voice cloning (from a recording).
//!
//! Voices are built as soon as they are submitted, so their owner and the reviewing admin can
//! hear the result, but none can speak in game until an admin approves it. The adapter owns the
//! registry because it holds the provider voice IDs; the game only sees opaque `custom:<id>` voices.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::Engine;
use reqwest::{Client, multipart};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::{
    audio::{AudioData, decode_recording, decode_wav, encode_ogg, encode_wav},
    config::Config,
    error::AppError,
    provider::{DashscopeClient, Synthesis},
    voices::{Gender, Voice},
};

/// Prefix of voice IDs the game uses for custom voices.
pub const CUSTOM_VOICE_PREFIX: &str = "custom:";
/// Provider-side voice name prefix: letters and digits, at most 10 characters.
const PROVIDER_PREFIX: &str = "ss13";
const MIN_RECORDING_SECONDS: f64 = 5.0;
const MAX_RECORDING_SECONDS: f64 = 60.0;
const MIN_RECORDING_SAMPLE_RATE: u32 = 16_000;
const MAX_PROMPT_CHARS: usize = 200;
const MAX_NAME_CHARS: usize = 24;
const DEFAULT_PREVIEW_TEXT: &str = "你好，这是我的声音。今天空间站的工作也要加油哦。";
const CLONE_READY_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Design,
    Clone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Built and waiting for an admin.
    Pending,
    Approved,
    Rejected,
}

impl Status {
    /// Whether the voice counts against its owner's limit.
    fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Approved)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomVoice {
    pub id: String,
    pub ckey: String,
    pub name: String,
    pub kind: Kind,
    pub status: Status,
    /// Provider voice ID. Never sent to the game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The consent statement the owner accepted when uploading a recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<String>,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CustomVoice {
    /// What the game may see.
    pub fn view(&self, has_preview: bool, has_recording: bool) -> Value {
        json!({
            "id": format!("{CUSTOM_VOICE_PREFIX}{}", self.id),
            "ckey": self.ckey,
            "name": self.name,
            "kind": self.kind,
            "status": self.status,
            "prompt": self.prompt,
            "consent": self.consent,
            "created_at": self.created_at,
            "reviewed_by": self.reviewed_by,
            "reason": self.reason,
            "has_preview": has_preview,
            "has_recording": has_recording,
        })
    }

    /// The voice, if an admin approved it.
    pub fn as_voice(&self) -> Option<Voice> {
        (self.status == Status::Approved)
            .then(|| self.provider_voice())
            .flatten()
    }

    /// The voice regardless of review, for previews.
    fn provider_voice(&self) -> Option<Voice> {
        let voice_id = self.voice_id.clone()?;
        Some(Voice {
            display: format!("{CUSTOM_VOICE_PREFIX}{}", self.id),
            api: voice_id,
            label: self.name.clone(),
            gender: Gender::Female,
            description: String::new(),
            language: None,
            random: false,
            // Cloned voices imitate a real person, so their audio carries the AIGC watermark.
            aigc: self.kind == Kind::Clone,
        })
    }
}

pub struct CustomVoices {
    directory: PathBuf,
    records: Mutex<Vec<CustomVoice>>,
    created_today: Mutex<HashMap<(String, u64), usize>>,
    enrollment: Enrollment,
    provider: DashscopeClient,
    limit: usize,
    daily_creations: usize,
}

impl CustomVoices {
    pub async fn new(config: &Config, provider: DashscopeClient) -> Result<Self, AppError> {
        let directory = config.data_dir.join("custom_voices");
        tokio::fs::create_dir_all(&directory).await?;
        let records = match tokio::fs::read(directory.join("registry.json")).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                AppError::Internal(format!("invalid custom voice registry: {error}"))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            directory,
            records: Mutex::new(records),
            created_today: Mutex::new(HashMap::new()),
            enrollment: Enrollment::new(config)?,
            provider,
            limit: config.custom_voice_limit,
            daily_creations: config.custom_voice_daily_creations,
        })
    }

    pub async fn list(&self, ckey: Option<&str>, status: Option<Status>) -> Vec<Value> {
        let records = self.records.lock().await;
        records
            .iter()
            .filter(|record| ckey.is_none_or(|ckey| record.ckey == ckey))
            .filter(|record| status.is_none_or(|status| record.status == status))
            .map(|record| self.view(record))
            .collect()
    }

    fn view(&self, record: &CustomVoice) -> Value {
        record.view(
            self.preview_path(&record.id).exists(),
            self.recording_path(&record.id).exists(),
        )
    }

    pub async fn voice(&self, display: &str) -> Option<Voice> {
        let id = display.strip_prefix(CUSTOM_VOICE_PREFIX)?;
        let records = self.records.lock().await;
        records
            .iter()
            .find(|record| record.id == id)
            .and_then(CustomVoice::as_voice)
    }

    pub async fn preview(&self, id: &str) -> Result<Vec<u8>, AppError> {
        let id = parse_id(id)?;
        tokio::fs::read(self.preview_path(&id))
            .await
            .map_err(|_| AppError::NotFound("这个音色没有试听音频".into()))
    }

    /// The uploaded recording behind a cloned voice, kept only until review.
    pub async fn recording(&self, id: &str) -> Result<Vec<u8>, AppError> {
        let id = parse_id(id)?;
        tokio::fs::read(self.recording_path(&id))
            .await
            .map_err(|_| AppError::NotFound("原录音已在审核后删除".into()))
    }

    /// Counts one voice creation against the player's daily allowance.
    async fn consume_daily(&self, ckey: &str) -> Result<(), AppError> {
        let mut created = self.created_today.lock().await;
        let used = created.entry((ckey.to_owned(), today())).or_default();
        if *used >= self.daily_creations {
            return Err(AppError::BadRequest(format!(
                "今天的定制音色生成次数已用完（每天 {} 次）",
                self.daily_creations
            )));
        }
        *used += 1;
        Ok(())
    }

    /// Speaks the preview sentence with a freshly built voice.
    async fn synthesize_preview(&self, voice: &Voice) -> Result<Vec<u8>, AppError> {
        let synthesis = Synthesis {
            text: DEFAULT_PREVIEW_TEXT,
            voice,
            style: None,
            sound: None,
            instruction: None,
            rate: None,
            pitch: None,
        };
        let wav = self
            .provider
            .synthesize(&self.provider.request_body(&synthesis))
            .await?;
        encode_ogg(&decode_wav(&wav)?)
    }

    /// Designs a voice from a description. The provider builds it right away (creation is
    /// free), so the owner and the reviewing admin can both hear it before approval.
    pub async fn design(
        &self,
        ckey: &str,
        name: &str,
        prompt: &str,
        preview_text: Option<&str>,
    ) -> Result<Value, AppError> {
        let prompt = validate_text_field(prompt, MAX_PROMPT_CHARS, "声音描述")?;
        let preview_text = match preview_text.map(str::trim).filter(|text| !text.is_empty()) {
            Some(text) => validate_text_field(text, 200, "试听文本")?,
            None => DEFAULT_PREVIEW_TEXT.to_owned(),
        };
        if preview_text.chars().count() < 15 {
            return Err(AppError::BadRequest("试听文本至少需要 15 个字".into()));
        }
        self.check_limit(ckey).await?;
        self.consume_daily(ckey).await?;

        let (voice_id, preview_wav) = self.enrollment.design(&prompt, &preview_text).await?;
        let id = new_id(ckey);
        let preview = decode_wav(&preview_wav).and_then(|audio| encode_ogg(&audio));
        match preview {
            Ok(ogg) => atomic_write(&self.preview_path(&id), &ogg).await?,
            Err(error) => warn!(%error, "voice design preview could not be converted"),
        }
        let record = CustomVoice {
            id,
            ckey: ckey.to_owned(),
            name: name.to_owned(),
            kind: Kind::Design,
            status: Status::Pending,
            voice_id: Some(voice_id),
            prompt: Some(prompt),
            consent: None,
            created_at: now(),
            reviewed_by: None,
            reason: None,
        };
        info!(id = record.id, ckey, "voice design submitted for review");
        self.insert(record).await
    }

    /// Clones a voice from a recording and speaks a preview with it. The recording is kept,
    /// as a compressed copy, until an admin has listened to it.
    pub async fn submit_recording(
        &self,
        ckey: &str,
        name: &str,
        consent: &str,
        bytes: Vec<u8>,
    ) -> Result<Value, AppError> {
        let consent = validate_text_field(consent, 300, "授权声明")?;
        self.check_limit(ckey).await?;
        let audio =
            tokio::task::spawn_blocking(move || decode_recording(bytes, MAX_RECORDING_SECONDS))
                .await
                .map_err(|error| AppError::Internal(format!("audio worker failed: {error}")))??;
        if audio.sample_rate < MIN_RECORDING_SAMPLE_RATE {
            return Err(AppError::BadRequest(format!(
                "录音采样率不能低于 {MIN_RECORDING_SAMPLE_RATE} Hz"
            )));
        }
        if audio.duration_seconds() < MIN_RECORDING_SECONDS {
            return Err(AppError::BadRequest(format!(
                "录音至少需要 {MIN_RECORDING_SECONDS} 秒"
            )));
        }
        self.consume_daily(ckey).await?;

        let (wav, recording_ogg) = tokio::task::spawn_blocking(move || encode_recording(&audio))
            .await
            .map_err(|error| AppError::Internal(format!("audio worker failed: {error}")))??;
        let voice_id = self.enrollment.clone_voice(wav).await?;
        let mut record = CustomVoice {
            id: new_id(ckey),
            ckey: ckey.to_owned(),
            name: name.to_owned(),
            kind: Kind::Clone,
            status: Status::Pending,
            voice_id: Some(voice_id.clone()),
            prompt: None,
            consent: Some(consent),
            created_at: now(),
            reviewed_by: None,
            reason: None,
        };
        let preview = match record.provider_voice() {
            Some(voice) => self.synthesize_preview(&voice).await,
            None => Err(AppError::Internal("cloned voice has no ID".into())),
        };
        let preview = match preview {
            Ok(preview) => preview,
            Err(error) => {
                self.enrollment.delete(&voice_id).await;
                return Err(error);
            }
        };
        atomic_write(&self.preview_path(&record.id), &preview).await?;
        atomic_write(&self.recording_path(&record.id), &recording_ogg).await?;
        record.status = Status::Pending;
        info!(id = record.id, ckey, "cloned voice submitted for review");
        self.insert(record).await
    }

    /// Approves a pending voice so its owner can speak with it.
    pub async fn approve(&self, id: &str, admin: &str) -> Result<Value, AppError> {
        let id = parse_id(id)?;
        let record = {
            let mut records = self.records.lock().await;
            let record = records
                .iter_mut()
                .find(|record| record.id == id)
                .ok_or_else(|| AppError::NotFound("找不到这个定制音色".into()))?;
            if record.status != Status::Pending {
                return Err(AppError::BadRequest("这个音色不在待审核状态".into()));
            }
            record.reviewed_by = Some(admin.to_owned());
            record.status = Status::Approved;
            record.clone()
        };
        self.save().await?;
        // The recording is biometric data: keep it only as long as the review needs it.
        let _ = tokio::fs::remove_file(self.recording_path(&id)).await;
        info!(id, admin, "custom voice approved");
        Ok(self.view(&record))
    }

    pub async fn reject(&self, id: &str, admin: &str, reason: &str) -> Result<Value, AppError> {
        let id = parse_id(id)?;
        let reason = validate_text_field(reason, 200, "拒绝理由")?;
        let (voice_id, view) = {
            let mut records = self.records.lock().await;
            let record = records
                .iter_mut()
                .find(|record| record.id == id)
                .ok_or_else(|| AppError::NotFound("找不到这个定制音色".into()))?;
            if record.status != Status::Pending {
                return Err(AppError::BadRequest("这个音色不在待审核状态".into()));
            }
            record.status = Status::Rejected;
            record.reviewed_by = Some(admin.to_owned());
            record.reason = Some(reason);
            (record.voice_id.take(), record.view(false, false))
        };
        self.save().await?;
        if let Some(voice_id) = voice_id {
            self.enrollment.delete(&voice_id).await;
        }
        let _ = tokio::fs::remove_file(self.recording_path(&id)).await;
        let _ = tokio::fs::remove_file(self.preview_path(&id)).await;
        info!(id, admin, "custom voice rejected");
        Ok(view)
    }

    /// Deletes a voice. With `owner`, only that player's voice may be deleted.
    pub async fn delete(&self, id: &str, owner: Option<&str>) -> Result<(), AppError> {
        let id = parse_id(id)?;
        let record = {
            let mut records = self.records.lock().await;
            let index = records
                .iter()
                .position(|record| {
                    record.id == id && owner.is_none_or(|owner| record.ckey == owner)
                })
                .ok_or_else(|| AppError::NotFound("找不到这个定制音色".into()))?;
            records.remove(index)
        };
        self.save().await?;
        if let Some(voice_id) = &record.voice_id {
            self.enrollment.delete(voice_id).await;
        }
        let _ = tokio::fs::remove_file(self.recording_path(&id)).await;
        let _ = tokio::fs::remove_file(self.preview_path(&id)).await;
        info!(id, "custom voice deleted");
        Ok(())
    }

    async fn check_limit(&self, ckey: &str) -> Result<(), AppError> {
        let records = self.records.lock().await;
        let active = records
            .iter()
            .filter(|record| record.ckey == ckey && record.status.is_active())
            .count();
        if active >= self.limit {
            return Err(AppError::BadRequest(format!(
                "每位玩家最多保留 {} 个定制音色，请先删除不用的",
                self.limit
            )));
        }
        Ok(())
    }

    async fn insert(&self, record: CustomVoice) -> Result<Value, AppError> {
        let view = self.view(&record);
        self.records.lock().await.push(record);
        self.save().await?;
        Ok(view)
    }

    async fn save(&self) -> Result<(), AppError> {
        let bytes = {
            let records = self.records.lock().await;
            serde_json::to_vec_pretty(&*records).map_err(|error| {
                AppError::Internal(format!("cannot serialize registry: {error}"))
            })?
        };
        atomic_write(&self.directory.join("registry.json"), &bytes).await
    }

    fn preview_path(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.ogg"))
    }

    fn recording_path(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}-recording.ogg"))
    }
}

fn encode_recording(audio: &AudioData) -> Result<(Vec<u8>, Vec<u8>), AppError> {
    Ok((encode_wav(audio)?, encode_ogg(audio)?))
}

/// The voice-enrollment API: voice design, cloning and deletion.
struct Enrollment {
    client: Client,
    api_key: String,
    endpoint: String,
    uploads_endpoint: String,
    target_model: String,
}

impl Enrollment {
    fn new(config: &Config) -> Result<Self, AppError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(90))
            .build()
            .map_err(|error| AppError::Internal(format!("failed to build HTTP client: {error}")))?;
        Ok(Self {
            client,
            api_key: config.dashscope_api_key.clone(),
            endpoint: config.customization_endpoint.clone(),
            uploads_endpoint: config.uploads_endpoint.clone(),
            target_model: config.model.clone(),
        })
    }

    async fn call(&self, input: Value, resolve_oss: bool) -> Result<Value, AppError> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&json!({ "model": "voice-enrollment", "input": input }));
        if resolve_oss {
            request = request.header("X-DashScope-OssResourceResolve", "enable");
        }
        let response = request
            .send()
            .await
            .map_err(|error| AppError::Provider(format!("voice enrollment failed: {error}")))?;
        let status = response.status();
        let payload: Value = response.json().await.map_err(|error| {
            AppError::Provider(format!("voice enrollment returned invalid JSON: {error}"))
        })?;
        if !status.is_success() || payload.get("output").is_none() {
            return Err(AppError::Provider(format!(
                "voice enrollment HTTP {status} ({}): {}",
                payload["code"].as_str().unwrap_or("unknown_error"),
                payload["message"].as_str().unwrap_or("no error message")
            )));
        }
        Ok(payload["output"].clone())
    }

    async fn design(
        &self,
        prompt: &str,
        preview_text: &str,
    ) -> Result<(String, Vec<u8>), AppError> {
        let request = json!({
            "action": "create_voice",
            "target_model": self.target_model,
            "voice_prompt": prompt,
            "preview_text": preview_text,
            "prefix": PROVIDER_PREFIX,
        });
        let mut body = json!({ "model": "voice-enrollment", "input": request });
        body["parameters"] = json!({ "sample_rate": 24_000, "response_format": "wav" });
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| AppError::Provider(format!("voice design failed: {error}")))?;
        let status = response.status();
        let payload: Value = response.json().await.map_err(|error| {
            AppError::Provider(format!("voice design returned invalid JSON: {error}"))
        })?;
        let output = &payload["output"];
        let voice_id = output["voice_id"].as_str().filter(|_| status.is_success());
        let Some(voice_id) = voice_id else {
            return Err(AppError::Provider(format!(
                "voice design HTTP {status} ({}): {}",
                payload["code"].as_str().unwrap_or("unknown_error"),
                payload["message"].as_str().unwrap_or("no error message")
            )));
        };
        let preview = output["preview_audio"]["data"]
            .as_str()
            .and_then(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
            .unwrap_or_default();
        Ok((voice_id.to_owned(), preview))
    }

    /// Uploads the recording to DashScope's temporary storage, clones it, and waits until
    /// the voice can synthesize.
    async fn clone_voice(&self, wav: Vec<u8>) -> Result<String, AppError> {
        let url = self.upload(wav).await?;
        let output = self
            .call(
                json!({
                    "action": "create_voice",
                    "target_model": self.target_model,
                    "prefix": PROVIDER_PREFIX,
                    "url": url,
                    "language_hints": ["zh"],
                }),
                true,
            )
            .await?;
        let voice_id = output["voice_id"]
            .as_str()
            .ok_or_else(|| AppError::Provider("voice cloning returned no voice ID".into()))?
            .to_owned();

        let started = tokio::time::Instant::now();
        loop {
            let output = self
                .call(
                    json!({ "action": "query_voice", "voice_id": voice_id }),
                    false,
                )
                .await?;
            match output["status"].as_str() {
                Some("OK") => return Ok(voice_id),
                Some("DEPLOYING") | None => {}
                Some(other) => {
                    self.delete(&voice_id).await;
                    return Err(AppError::Provider(format!(
                        "voice cloning ended as {other}"
                    )));
                }
            }
            if started.elapsed() > CLONE_READY_TIMEOUT {
                self.delete(&voice_id).await;
                return Err(AppError::Provider("voice cloning timed out".into()));
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }

    async fn upload(&self, wav: Vec<u8>) -> Result<String, AppError> {
        let policy: Value = self
            .client
            .get(format!(
                "{}?action=getPolicy&model=voice-enrollment",
                self.uploads_endpoint
            ))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|error| AppError::Provider(format!("upload policy request failed: {error}")))?
            .json()
            .await
            .map_err(|error| AppError::Provider(format!("invalid upload policy: {error}")))?;
        let policy = &policy["data"];
        let field = |name: &str| -> Result<String, AppError> {
            policy[name]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| AppError::Provider(format!("upload policy lacks {name}")))
        };
        let key = format!("{}/recording.wav", field("upload_dir")?);
        let form = multipart::Form::new()
            .text("OSSAccessKeyId", field("oss_access_key_id")?)
            .text("Signature", field("signature")?)
            .text("policy", field("policy")?)
            .text("x-oss-object-acl", field("x_oss_object_acl")?)
            .text("x-oss-forbid-overwrite", field("x_oss_forbid_overwrite")?)
            .text("key", key.clone())
            .text("success_action_status", "200")
            .part(
                "file",
                multipart::Part::bytes(wav)
                    .file_name("recording.wav")
                    .mime_str("audio/wav")
                    .map_err(|error| AppError::Internal(format!("invalid MIME type: {error}")))?,
            );
        let response = self
            .client
            .post(field("upload_host")?)
            .multipart(form)
            .send()
            .await
            .map_err(|error| AppError::Provider(format!("recording upload failed: {error}")))?;
        if !response.status().is_success() {
            return Err(AppError::Provider(format!(
                "recording upload returned HTTP {}",
                response.status()
            )));
        }
        Ok(format!("oss://{key}"))
    }

    /// Best-effort deletion: a leftover voice only costs quota and expires after a year unused.
    async fn delete(&self, voice_id: &str) {
        if let Err(error) = self
            .call(
                json!({ "action": "delete_voice", "voice_id": voice_id }),
                false,
            )
            .await
        {
            warn!(voice_id, %error, "failed to delete provider voice");
        }
    }
}

/// Validates free text from players: trimmed, no control characters or markup, bounded length.
fn validate_text_field(text: &str, max_chars: usize, field: &str) -> Result<String, AppError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::BadRequest(format!("{field}不能为空")));
    }
    if text.chars().count() > max_chars {
        return Err(AppError::BadRequest(format!(
            "{field}不能超过 {max_chars} 个字"
        )));
    }
    if text
        .chars()
        .any(|character| character.is_control() || matches!(character, '<' | '>' | '[' | ']'))
    {
        return Err(AppError::BadRequest(format!(
            "{field}不能包含控制字符、尖括号或方括号"
        )));
    }
    Ok(text.to_owned())
}

pub fn validate_name(name: &str) -> Result<String, AppError> {
    validate_text_field(name, MAX_NAME_CHARS, "音色名称")
}

pub fn validate_ckey(ckey: &str) -> Result<String, AppError> {
    if ckey.is_empty()
        || ckey.len() > 32
        || !ckey
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(AppError::BadRequest("invalid ckey".into()));
    }
    Ok(ckey.to_owned())
}

fn parse_id(id: &str) -> Result<String, AppError> {
    let id = id.strip_prefix(CUSTOM_VOICE_PREFIX).unwrap_or(id);
    if id.len() != 16 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::BadRequest("invalid custom voice id".into()));
    }
    Ok(id.to_ascii_lowercase())
}

fn new_id(ckey: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut hash = Sha256::new();
    hash.update(ckey.as_bytes());
    hash.update(nanos.to_le_bytes());
    hash.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    hash.update(std::process::id().to_le_bytes());
    hex::encode(&hash.finalize()[..8])
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn today() -> u64 {
    now() / 86_400
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    tokio::fs::write(&temporary, bytes).await?;
    tokio::fs::rename(&temporary, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_validated() {
        let id = new_id("player");
        assert_eq!(parse_id(&format!("custom:{id}")).unwrap(), id);
        assert!(parse_id("custom:../../etc").is_err());
        assert!(parse_id("1234").is_err());
    }

    #[test]
    fn player_text_is_validated() {
        assert!(validate_text_field("低沉的中年男声", 200, "d").is_ok());
        assert!(validate_text_field("  ", 200, "d").is_err());
        assert!(validate_text_field("[laughing]", 200, "d").is_err());
        assert!(validate_text_field(&"字".repeat(201), 200, "d").is_err());
        assert!(validate_ckey("player1").is_ok());
        assert!(validate_ckey("Player 1").is_err());
    }

    #[test]
    fn only_approved_voices_can_speak() {
        let mut record = CustomVoice {
            id: new_id("player"),
            ckey: "player".into(),
            name: "测试".into(),
            kind: Kind::Clone,
            status: Status::Pending,
            voice_id: Some("provider-voice".into()),
            prompt: None,
            consent: Some("本人声音".into()),
            created_at: 0,
            reviewed_by: None,
            reason: None,
        };
        assert!(record.as_voice().is_none());
        record.status = Status::Approved;
        let voice = record.as_voice().unwrap();
        assert_eq!(voice.api, "provider-voice");
        assert!(voice.aigc, "cloned voices must carry the AIGC watermark");
        assert!(record.view(false, false).get("voice_id").is_none());
    }
}
