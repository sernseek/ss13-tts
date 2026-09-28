use std::sync::Arc;

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::Path,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq;
use tracing::info;

use crate::{
    audio::{apply_effects, encode_ogg, make_blips},
    custom_voices::{CustomVoices, Status, validate_ckey, validate_name},
    error::AppError,
    provider::{SOUND_TAGS, STYLE_TAGS, Synthesis},
    state::AppState,
    voices::Voice,
};

/// Largest recording accepted for voice cloning.
const MAX_RECORDING_BYTES: usize = 10 * 1024 * 1024;

/// Instruction length limit, counting CJK characters twice as the provider does.
const MAX_INSTRUCTION_WEIGHT: usize = 100;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health-check", get(health_check))
        .route("/tts-voices", get(voices))
        .route("/tts-voice-info", get(voice_info))
        .route("/custom-voices", get(custom_voice_list))
        .route("/custom-voices/design", post(custom_voice_design))
        .route(
            "/custom-voices/recording",
            post(custom_voice_recording).layer(DefaultBodyLimit::max(MAX_RECORDING_BYTES)),
        )
        .route("/custom-voices/{id}/preview", get(custom_voice_preview))
        .route(
            "/custom-voices/{id}/recording",
            get(custom_voice_recording_audio),
        )
        .route("/custom-voices/{id}/approve", post(custom_voice_approve))
        .route("/custom-voices/{id}/reject", post(custom_voice_reject))
        .route("/custom-voices/{id}/delete", post(custom_voice_delete))
        .route("/pitch-available", get(pitch_available))
        .route("/tts", get(tts))
        .route("/tts-blips", get(tts_blips))
        .route("/tts-radio", get(tts_radio))
        .route("/tts-blips-radio", get(tts_blips_radio))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .with_state(state)
}

async fn health_check() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn voices(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<String>>, AppError> {
    authorize(&state, &headers)?;
    Ok(Json(
        state
            .voices
            .voices()
            .iter()
            .map(|voice| voice.display.clone())
            .collect(),
    ))
}

/// Describes voices and optional synthesis features for clients that understand them.
async fn voice_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    let expressive = state.config.backend.supports_expression();
    let aliases: serde_json::Map<String, serde_json::Value> = state
        .voices
        .aliases()
        .map(|(old, new)| (old.to_owned(), json!(new)))
        .collect();
    Ok(Json(json!({
        "model": state.config.model,
        "voices": state.voices.voices(),
        "aliases": aliases,
        "styles": if expressive { STYLE_TAGS } else { &[] },
        "sounds": if expressive { SOUND_TAGS } else { &[] },
        "instruction": expressive,
        "rate": expressive,
        "pitch": expressive,
        "custom_voices": state.custom_voices.is_some(),
    }))
    .into_response())
}

fn custom_voices(state: &AppState) -> Result<&Arc<CustomVoices>, AppError> {
    state.custom_voices.as_ref().ok_or_else(|| {
        AppError::NotFound("the configured TTS model does not support custom voices".into())
    })
}

#[derive(Debug, Deserialize)]
struct CustomVoiceQuery {
    ckey: Option<String>,
    status: Option<Status>,
    name: Option<String>,
    admin: Option<String>,
    reason: Option<String>,
}

impl CustomVoiceQuery {
    fn ckey(&self) -> Result<String, AppError> {
        validate_ckey(self.ckey.as_deref().unwrap_or_default())
    }

    fn admin(&self) -> Result<String, AppError> {
        validate_ckey(self.admin.as_deref().unwrap_or_default())
    }

    fn name(&self) -> Result<String, AppError> {
        validate_name(self.name.as_deref().unwrap_or_default())
    }
}

#[derive(Debug, Deserialize)]
struct DesignBody {
    prompt: String,
    preview_text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecordingBody {
    consent: String,
}

/// Lists player-made voices, optionally for one player or in one status.
async fn custom_voice_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<CustomVoiceQuery>,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    let ckey = query.ckey.as_deref().map(validate_ckey).transpose()?;
    let voices = custom_voices(&state)?
        .list(ckey.as_deref(), query.status)
        .await;
    Ok(Json(voices).into_response())
}

async fn custom_voice_design(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<CustomVoiceQuery>,
    body: Bytes,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    let body: DesignBody = serde_json::from_slice(&body)
        .map_err(|error| AppError::BadRequest(format!("invalid JSON body: {error}")))?;
    let voice = custom_voices(&state)?
        .design(
            &query.ckey()?,
            &query.name()?,
            &body.prompt,
            body.preview_text.as_deref(),
        )
        .await?;
    Ok(Json(voice).into_response())
}

/// Receives a recording for voice cloning. The body is the audio file itself; the consent
/// statement travels in the `X-Voice-Consent` header as URL-encoded JSON.
async fn custom_voice_recording(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<CustomVoiceQuery>,
    body: Bytes,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    let consent = headers
        .get("x-voice-consent")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            serde_urlencoded::from_str::<RecordingBody>(value)
                .ok()
                .map(|body| body.consent)
        })
        .ok_or_else(|| AppError::BadRequest("missing consent statement".into()))?;
    let voice = custom_voices(&state)?
        .submit_recording(&query.ckey()?, &query.name()?, &consent, body.to_vec())
        .await?;
    Ok(Json(voice).into_response())
}

async fn custom_voice_preview(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    let ogg = custom_voices(&state)?.preview(&id).await?;
    let mut response = Response::new(Body::from(ogg));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/ogg"));
    Ok(response)
}

/// The recording behind a cloned voice, for the reviewing admin.
async fn custom_voice_recording_audio(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    let ogg = custom_voices(&state)?.recording(&id).await?;
    let mut response = Response::new(Body::from(ogg));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/ogg"));
    Ok(response)
}

async fn custom_voice_approve(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<CustomVoiceQuery>,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    let voice = custom_voices(&state)?.approve(&id, &query.admin()?).await?;
    Ok(Json(voice).into_response())
}

async fn custom_voice_reject(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<CustomVoiceQuery>,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    let reason = query.reason.as_deref().unwrap_or("未说明原因");
    let voice = custom_voices(&state)?
        .reject(&id, &query.admin()?, reason)
        .await?;
    Ok(Json(voice).into_response())
}

/// Deletes a voice. With `ckey`, only that player's own voice.
async fn custom_voice_delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<CustomVoiceQuery>,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    let owner = query.ckey.as_deref().map(validate_ckey).transpose()?;
    custom_voices(&state)?.delete(&id, owner.as_deref()).await?;
    Ok(Json(json!({ "deleted": true })).into_response())
}

async fn pitch_available(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    if state.config.backend.supports_expression() {
        Ok((StatusCode::OK, "pitch shifting is available").into_response())
    } else {
        Ok((StatusCode::NOT_FOUND, "pitch shifting is disabled").into_response())
    }
}

async fn tts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<TtsQuery>,
    body: Bytes,
) -> Result<Response, AppError> {
    audio_response(state, headers, query, body, Variant::Speech).await
}

async fn tts_blips(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<TtsQuery>,
    body: Bytes,
) -> Result<Response, AppError> {
    audio_response(state, headers, query, body, Variant::Blips).await
}

async fn tts_radio(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<TtsQuery>,
    body: Bytes,
) -> Result<Response, AppError> {
    audio_response(state, headers, query, body, Variant::Radio).await
}

async fn tts_blips_radio(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<TtsQuery>,
    body: Bytes,
) -> Result<Response, AppError> {
    audio_response(state, headers, query, body, Variant::RadioBlips).await
}

async fn audio_response(
    state: Arc<AppState>,
    headers: HeaderMap,
    query: TtsQuery,
    body: Bytes,
    variant: Variant,
) -> Result<Response, AppError> {
    authorize(&state, &headers)?;
    validate_identifier(&query.identifier)?;
    let voice: Voice = state
        .voice(&query.voice)
        .await
        .ok_or_else(|| AppError::BadRequest("unknown voice".into()))?;
    let body: TtsBody = serde_json::from_slice(&body)
        .map_err(|error| AppError::BadRequest(format!("invalid JSON body: {error}")))?;
    let style = allowlisted(body.style.as_deref(), STYLE_TAGS, "style")?;
    let sound = allowlisted(body.sound.as_deref(), SOUND_TAGS, "sound")?;
    let instruction = body
        .instruction
        .map(|instruction| instruction.trim().to_owned())
        .filter(|instruction| !instruction.is_empty());
    if let Some(instruction) = &instruction {
        validate_instruction(instruction)?;
    }
    let rate = body.rate.filter(|rate| *rate != 1.0);
    if rate.is_some_and(|rate| !(0.5..=2.0).contains(&rate)) {
        return Err(AppError::BadRequest("rate must be within 0.5-2.0".into()));
    }
    let pitch = pitch_factor(query.pitch.as_deref())?;
    let text = body
        .text
        .or(body.gibberish_text)
        .or(body.raw_text)
        .ok_or_else(|| AppError::BadRequest("JSON body must contain text".into()))?;
    // A nonverbal sound needs no words: emotes send just the sound tag.
    if !(sound.is_some() && text.trim().is_empty()) {
        validate_text(&text, state.config.max_text_chars)?;
    }

    info!(
        identifier = query.identifier,
        voice = query.voice,
        text_characters = text.chars().count(),
        variant = variant.name(),
        style = style.unwrap_or_default(),
        sound = sound.unwrap_or_default(),
        "processing TTS request"
    );

    let special_filters = query.special_filters.as_deref().unwrap_or_default();
    let silicon = special_filters.split('|').any(|value| value == "silicon");
    let legacy_filter = query
        .filter
        .as_deref()
        .is_some_and(|value| !value.is_empty());
    let radio = matches!(variant, Variant::Radio | Variant::RadioBlips)
        || special_filters.split('|').any(|value| value == "radio");

    let audio = match variant {
        Variant::Blips | Variant::RadioBlips => make_blips(
            &text,
            &voice.display,
            query.blip_base.as_deref().unwrap_or("male"),
            query.blip_number.as_deref().unwrap_or("1"),
        ),
        Variant::Speech | Variant::Radio => {
            let expressive = state.config.backend.supports_expression();
            let synthesis = Synthesis {
                text: &text,
                voice: &voice,
                style: style.filter(|_| expressive),
                sound: sound.filter(|_| expressive),
                instruction: instruction.as_deref().filter(|_| expressive),
                rate: rate.filter(|_| expressive),
                pitch: pitch.filter(|_| expressive),
            };
            let base = state.base_audio(&synthesis).await?;
            (*base).clone()
        }
    };
    let seed = query.identifier.clone();
    let encoded = tokio::task::spawn_blocking(move || {
        let processed = apply_effects(&audio, legacy_filter, silicon, radio, &seed);
        let duration = processed.duration_seconds();
        let ogg = encode_ogg(&processed)?;
        Ok::<_, AppError>((ogg, duration))
    })
    .await
    .map_err(|error| AppError::Internal(format!("audio worker failed: {error}")))??;

    ogg_response(encoded.0, encoded.1)
}

fn ogg_response(bytes: Vec<u8>, duration: f64) -> Result<Response, AppError> {
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/ogg"));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=tts.ogg"),
    );
    response.headers_mut().insert(
        "audio-length",
        HeaderValue::from_str(&format!("{duration:.3}")).map_err(|error| {
            AppError::Internal(format!("invalid audio duration header: {error}"))
        })?,
    );
    Ok(response)
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let provided = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let expected = state.config.authorization_token.as_bytes();
    let authorized =
        expected.len() == provided.len() && bool::from(expected.ct_eq(provided.as_bytes()));
    if authorized {
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}

fn validate_identifier(identifier: &str) -> Result<(), AppError> {
    if identifier.is_empty()
        || identifier.len() > 128
        || !identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(AppError::BadRequest("invalid identifier".into()));
    }
    Ok(())
}

fn validate_text(text: &str, max_chars: usize) -> Result<(), AppError> {
    let length = text.chars().count();
    if text.trim().is_empty() {
        return Err(AppError::BadRequest("text cannot be empty".into()));
    }
    if text
        .chars()
        .any(|character| character.is_control() && !character.is_whitespace())
    {
        return Err(AppError::BadRequest(
            "text contains control characters".into(),
        ));
    }
    if length > max_chars {
        return Err(AppError::BadRequest(format!(
            "text exceeds the {max_chars}-character limit"
        )));
    }
    Ok(())
}

/// Maps a requested tag onto the allowlist, so only known tags ever reach the provider.
fn allowlisted(
    requested: Option<&str>,
    allowlist: &[&'static str],
    field: &str,
) -> Result<Option<&'static str>, AppError> {
    let Some(requested) = requested.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    allowlist
        .iter()
        .copied()
        .find(|tag| *tag == requested)
        .map(Some)
        .ok_or_else(|| AppError::BadRequest(format!("unknown {field} tag")))
}

fn validate_instruction(instruction: &str) -> Result<(), AppError> {
    if instruction.chars().any(char::is_control) {
        return Err(AppError::BadRequest(
            "instruction contains control characters".into(),
        ));
    }
    let weight: usize = instruction
        .chars()
        .map(|character| if is_cjk(character) { 2 } else { 1 })
        .sum();
    if weight > MAX_INSTRUCTION_WEIGHT {
        return Err(AppError::BadRequest(format!(
            "instruction exceeds {MAX_INSTRUCTION_WEIGHT} characters"
        )));
    }
    Ok(())
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F
    )
}

/// Converts the game's semitone offset into the provider's pitch multiplier.
fn pitch_factor(semitones: Option<&str>) -> Result<Option<f32>, AppError> {
    let Some(semitones) = semitones.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let semitones: f32 = semitones
        .parse()
        .map_err(|_| AppError::BadRequest("pitch must be a number".into()))?;
    if !semitones.is_finite() {
        return Err(AppError::BadRequest("pitch must be a number".into()));
    }
    let semitones = semitones.clamp(-12.0, 12.0);
    if semitones.abs() < 0.01 {
        return Ok(None);
    }
    let factor = 2_f32.powf(semitones / 12.0).clamp(0.5, 2.0);
    Ok(Some((factor * 100.0).round() / 100.0))
}

#[derive(Debug, Deserialize)]
struct TtsQuery {
    voice: String,
    identifier: String,
    filter: Option<String>,
    special_filters: Option<String>,
    blip_base: Option<String>,
    blip_number: Option<String>,
    pitch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TtsBody {
    text: Option<String>,
    raw_text: Option<String>,
    gibberish_text: Option<String>,
    style: Option<String>,
    sound: Option<String>,
    instruction: Option<String>,
    rate: Option<f32>,
}

#[derive(Clone, Copy)]
enum Variant {
    Speech,
    Blips,
    Radio,
    RadioBlips,
}

impl Variant {
    fn name(self) -> &'static str {
        match self {
            Self::Speech => "speech",
            Self::Blips => "blips",
            Self::Radio => "radio",
            Self::RadioBlips => "radio-blips",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_maps_semitones_to_multiplier() {
        assert_eq!(pitch_factor(None).unwrap(), None);
        assert_eq!(pitch_factor(Some("0")).unwrap(), None);
        assert_eq!(pitch_factor(Some("12")).unwrap(), Some(2.0));
        assert_eq!(pitch_factor(Some("-12")).unwrap(), Some(0.5));
        assert_eq!(pitch_factor(Some("40")).unwrap(), Some(2.0));
        assert!(pitch_factor(Some("NaN")).is_err());
    }

    #[test]
    fn instruction_counts_cjk_twice() {
        assert!(validate_instruction(&"字".repeat(50)).is_ok());
        assert!(validate_instruction(&"字".repeat(51)).is_err());
        assert!(validate_instruction("a\u{7}").is_err());
    }

    #[test]
    fn tags_must_be_allowlisted() {
        assert_eq!(
            allowlisted(Some("shouting"), STYLE_TAGS, "style").unwrap(),
            Some("shouting")
        );
        assert!(allowlisted(Some("shouting]x[laughing"), STYLE_TAGS, "style").is_err());
        assert_eq!(allowlisted(Some(""), SOUND_TAGS, "sound").unwrap(), None);
    }
}
