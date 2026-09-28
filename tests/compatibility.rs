use std::{
    f32::consts::TAU,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode, header},
    routing::{get, post},
};
use serde_json::{Value, json};
use ss13_tts::{AppState, Config, provider::Backend, router};
use tempfile::TempDir;
use tower::ServiceExt;

#[tokio::test]
async fn implements_tgstation_contract_and_deduplicates_synthesis() {
    let mock = MockDashscope::start().await;
    let cache = TempDir::new().expect("temporary cache");
    let app = router(
        AppState::new(test_config(&mock, &cache, "qwen3-tts-flash-test"))
            .await
            .expect("app state"),
    );

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/tts-voices").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let voices = app
        .clone()
        .oneshot(authenticated_request("/tts-voices", Body::empty()))
        .await
        .unwrap();
    assert_eq!(voices.status(), StatusCode::OK);
    let voice_body = to_bytes(voices.into_body(), 64 * 1024).await.unwrap();
    let voice_list: Vec<String> = serde_json::from_slice(&voice_body).unwrap();
    assert!(voice_list.iter().any(|voice| voice.contains("Woman")));
    assert!(voice_list.iter().any(|voice| voice.contains("Man")));

    let cases = [
        (
            "/tts",
            r#"{"text":"你好，空间站！"}"#,
            "&special_filters=silicon&filter=megaphone",
        ),
        ("/tts-radio", r#"{"text":"你好，空间站！"}"#, ""),
        ("/tts-blips", r#"{"text":"你好，空间站！"}"#, ""),
        ("/tts-blips-radio", r#"{"text":"你好，空间站！"}"#, ""),
        (
            "/tts-radio",
            r#"{"raw_text":"你好，空间站！","gibberish_text":"你好，空间站！"}"#,
            "",
        ),
    ];
    for (route, body, extra_query) in cases {
        let uri = format!(
            "{route}?voice=Cherry%20Woman&identifier=compatibility.1&pitch=0&blip_base=female&blip_number=1{extra_query}"
        );
        let response = app
            .clone()
            .oneshot(authenticated_request(&uri, Body::from(body)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "route {route}");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/ogg");
        assert!(response.headers().contains_key("audio-length"));
        let audio = to_bytes(response.into_body(), 5 * 1024 * 1024)
            .await
            .unwrap();
        assert!(audio.starts_with(b"OggS"), "route {route}");
    }

    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);

    let pitch = app
        .oneshot(authenticated_request("/pitch-available", Body::empty()))
        .await
        .unwrap();
    assert_eq!(pitch.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn qwen_audio_forwards_expressive_controls() {
    let mock = MockDashscope::start().await;
    let cache = TempDir::new().expect("temporary cache");
    let app = router(
        AppState::new(test_config(&mock, &cache, "qwen-audio-3.1-tts-flash"))
            .await
            .expect("app state"),
    );

    let info = app
        .clone()
        .oneshot(authenticated_request("/tts-voice-info", Body::empty()))
        .await
        .unwrap();
    assert_eq!(info.status(), StatusCode::OK);
    let info: Value =
        serde_json::from_slice(&to_bytes(info.into_body(), 256 * 1024).await.unwrap()).unwrap();
    assert_eq!(info["aliases"]["Cherry Woman"], "Yu Xiaoyun Woman");
    assert!(
        info["styles"]
            .as_array()
            .unwrap()
            .contains(&json!("shouting"))
    );
    assert!(
        info["sounds"]
            .as_array()
            .unwrap()
            .contains(&json!("laughing"))
    );
    let first_voice = &info["voices"][0];
    assert!(first_voice["id"].is_string() && first_voice["label"].is_string());
    assert!(first_voice.get("api").is_none());

    let pitch = app
        .clone()
        .oneshot(authenticated_request("/pitch-available", Body::empty()))
        .await
        .unwrap();
    assert_eq!(pitch.status(), StatusCode::OK);

    let body = r#"{"text":"[laughing]你好","style":"shouting","sound":"laughing","instruction":"沙哑的老年男性","rate":1.25}"#;
    let response = app
        .clone()
        .oneshot(authenticated_request(
            "/tts?voice=Cherry%20Woman&identifier=expressive.1&pitch=12",
            Body::from(body),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let input = mock.last_payload.lock().unwrap().clone().unwrap()["input"].clone();
    assert_eq!(input["text"], "[laughing][shouting](laughing)你好");
    assert_eq!(input["voice"], "yuxiaoyun_v3.1");
    assert_eq!(input["instruction"], "沙哑的老年男性");
    assert_eq!(input["rate"], 1.25);
    assert_eq!(input["pitch"], 2.0);
    assert_eq!(input["format"], "wav");

    for (body, reason) in [
        (
            r#"{"text":"你好","style":"laughing"}"#,
            "sound used as style",
        ),
        (r#"{"text":"你好","sound":"[x]"}"#, "unknown sound"),
        (r#"{"text":"你好","rate":3}"#, "rate out of range"),
    ] {
        let response = app
            .clone()
            .oneshot(authenticated_request(
                "/tts?voice=Anyang%20Man&identifier=expressive.2",
                Body::from(body),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{reason}");
    }
}

#[tokio::test]
async fn custom_voices_require_review_and_ownership() {
    let mock = MockDashscope::start().await;
    let cache = TempDir::new().expect("temporary cache");
    let app = router(
        AppState::new(test_config(&mock, &cache, "qwen-audio-3.1-tts-flash"))
            .await
            .expect("app state"),
    );
    let call = |uri: String, body: Body| {
        let app = app.clone();
        async move {
            let response = app
                .oneshot(authenticated_request(&uri, body))
                .await
                .unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            (status, bytes)
        }
    };
    let post = |uri: &str, body: Body| {
        Request::post(uri)
            .header("authorization", "test-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap()
    };

    let design = app
        .clone()
        .oneshot(post(
            "/custom-voices/design?ckey=donor&name=%E8%88%B0%E9%95%BF",
            Body::from(r#"{"prompt":"低沉沙哑的中年男性"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(design.status(), StatusCode::OK);
    let design: Value =
        serde_json::from_slice(&to_bytes(design.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(design["status"], "pending");
    assert_eq!(design["name"], "舰长");
    assert!(
        design.get("voice_id").is_none(),
        "provider IDs stay inside the adapter"
    );
    let id = design["id"].as_str().unwrap().to_owned();
    let speak = |voice: String| {
        call(
            format!(
                "/tts?voice={}&identifier=custom.1",
                voice.replace(':', "%3A")
            ),
            Body::from(r#"{"text":"你好"}"#),
        )
    };
    assert_eq!(
        speak(id.clone()).await.0,
        StatusCode::BAD_REQUEST,
        "pending voices cannot speak"
    );

    let (status, preview) = call(format!("/custom-voices/{id}/preview"), Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(preview.starts_with(b"OggS"));

    let approve = app
        .clone()
        .oneshot(post(
            &format!("/custom-voices/{id}/approve?admin=boss"),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(approve.status(), StatusCode::OK);
    assert_eq!(
        speak(id.clone()).await.0,
        StatusCode::OK,
        "approved voices speak"
    );
    let input = mock.last_payload.lock().unwrap().clone().unwrap()["input"].clone();
    assert_eq!(input["voice"], "qwen-audio-3.1-tts-flash-vd-ss13-mock");
    assert!(
        input.get("enable_aigc_tag").is_none(),
        "designed voices need no watermark"
    );

    let recording = app
        .clone()
        .oneshot(
            Request::post("/custom-voices/recording?ckey=donor&name=%E6%9C%AC%E4%BA%BA")
                .header("authorization", "test-token")
                .header(
                    "x-voice-consent",
                    "consent=%E6%9C%AC%E4%BA%BA%E5%A3%B0%E9%9F%B3",
                )
                .body(Body::from(long_wav(6)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(recording.status(), StatusCode::OK);
    let recording: Value =
        serde_json::from_slice(&to_bytes(recording.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(recording["kind"], "clone");
    assert_eq!(recording["consent"], "本人声音");
    assert_eq!(
        recording["has_preview"], true,
        "the owner can hear the clone before review"
    );
    assert_eq!(
        recording["has_recording"], true,
        "the admin can hear the original"
    );
    let recording_id = recording["id"].as_str().unwrap().to_owned();
    let (status, original) = call(
        format!("/custom-voices/{recording_id}/recording"),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(original.starts_with(b"OggS"));

    let over_limit = app
        .clone()
        .oneshot(post(
            "/custom-voices/design?ckey=donor&name=x",
            Body::from(r#"{"prompt":"清脆的少女声音"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(
        over_limit.status(),
        StatusCode::BAD_REQUEST,
        "per-player limit"
    );

    let short = app
        .clone()
        .oneshot(
            Request::post("/custom-voices/recording?ckey=other&name=x")
                .header("authorization", "test-token")
                .header("x-voice-consent", "consent=ok")
                .body(Body::from(long_wav(2)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        short.status(),
        StatusCode::BAD_REQUEST,
        "recordings under 5 s are refused"
    );

    let reject = app
        .clone()
        .oneshot(post(
            &format!("/custom-voices/{recording_id}/reject?admin=boss&reason=%E5%99%AA%E9%9F%B3"),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(reject.status(), StatusCode::OK);
    for file in ["preview", "recording"] {
        let (status, _) = call(
            format!("/custom-voices/{recording_id}/{file}"),
            Body::empty(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "rejected voices leave no {file}"
        );
    }

    let clone = app
        .clone()
        .oneshot(
            Request::post("/custom-voices/recording?ckey=donor&name=clone")
                .header("authorization", "test-token")
                .header("x-voice-consent", "consent=ok")
                .body(Body::from(long_wav(6)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(clone.status(), StatusCode::OK);
    let clone: Value =
        serde_json::from_slice(&to_bytes(clone.into_body(), 64 * 1024).await.unwrap()).unwrap();
    let clone_id = clone["id"].as_str().unwrap().to_owned();
    let approve = app
        .clone()
        .oneshot(post(
            &format!("/custom-voices/{clone_id}/approve?admin=boss"),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(approve.status(), StatusCode::OK);
    let (status, _) = call(
        format!("/custom-voices/{clone_id}/recording"),
        Body::empty(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the recording is deleted once reviewed"
    );
    assert_eq!(speak(clone_id.clone()).await.0, StatusCode::OK);
    let input = mock.last_payload.lock().unwrap().clone().unwrap()["input"].clone();
    assert_eq!(input["voice"], "qwen-audio-3.1-tts-flash-ss13-clone");
    assert_eq!(
        input["enable_aigc_tag"], true,
        "cloned voices carry the AIGC watermark"
    );

    let (status, list) = call("/custom-voices?ckey=donor".into(), Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<Value> = serde_json::from_slice(&list).unwrap();
    assert_eq!(list.len(), 3);
    assert!(
        list.iter()
            .any(|voice| voice["status"] == "rejected" && voice["reason"] == "噪音")
    );

    let stolen = app
        .clone()
        .oneshot(post(
            &format!("/custom-voices/{id}/delete?ckey=thief"),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(
        stolen.status(),
        StatusCode::NOT_FOUND,
        "only the owner may delete"
    );
    let deleted = app
        .clone()
        .oneshot(post(
            &format!("/custom-voices/{id}/delete?ckey=donor"),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(
        speak(id).await.0,
        StatusCode::BAD_REQUEST,
        "deleted voices are gone"
    );

    let laugh = call(
        "/tts?voice=Anyang%20Man&identifier=emote.1".into(),
        Body::from(r#"{"text":"","sound":"laughing"}"#),
    )
    .await;
    assert_eq!(laugh.0, StatusCode::OK, "emotes send only a sound tag");
    let input = mock.last_payload.lock().unwrap().clone().unwrap()["input"].clone();
    assert_eq!(input["text"], "[laughing]");
}

/// A 16 kHz mono WAV of the given length, long enough to pass the recording checks.
fn long_wav(seconds: u32) -> Vec<u8> {
    let sample_rate = 16_000_u32;
    let frames = sample_rate * seconds;
    let data_size = frames * 2;
    let mut output = Vec::new();
    output.extend_from_slice(b"RIFF");
    output.extend_from_slice(&(36 + data_size).to_le_bytes());
    output.extend_from_slice(b"WAVEfmt ");
    output.extend_from_slice(&16_u32.to_le_bytes());
    output.extend_from_slice(&1_u16.to_le_bytes());
    output.extend_from_slice(&1_u16.to_le_bytes());
    output.extend_from_slice(&sample_rate.to_le_bytes());
    output.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    output.extend_from_slice(&2_u16.to_le_bytes());
    output.extend_from_slice(&16_u16.to_le_bytes());
    output.extend_from_slice(b"data");
    output.extend_from_slice(&data_size.to_le_bytes());
    for index in 0..frames {
        let sample = (TAU * 180.0 * index as f32 / sample_rate as f32).sin() * 0.3;
        output.extend_from_slice(&((sample * i16::MAX as f32) as i16).to_le_bytes());
    }
    output
}

fn test_config(mock: &MockDashscope, cache: &TempDir, model: &str) -> Config {
    Config {
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        authorization_token: "test-token".into(),
        dashscope_api_key: "test-key".into(),
        dashscope_endpoint: mock.endpoint.clone(),
        model: model.into(),
        backend: Backend::for_model(model),
        language_type: "Auto".into(),
        language_hint: None,
        hot_fix: None,
        custom_voices: None,
        data_dir: cache.path().join("data"),
        customization_endpoint: mock.endpoint.replace("/generate", "/customization"),
        uploads_endpoint: mock.endpoint.replace("/generate", "/uploads"),
        custom_voice_limit: 2,
        custom_voice_daily_creations: 5,
        provider_timeout: Duration::from_secs(5),
        provider_concurrency: 3,
        cache_dir: cache.path().into(),
        cache_max_bytes: 10 * 1024 * 1024,
        cache_ttl: Duration::from_secs(60),
        max_text_chars: 1_200,
        allow_untrusted_audio_urls: true,
    }
}

fn authenticated_request(uri: &str, body: Body) -> Request<Body> {
    Request::get(uri)
        .header("authorization", "test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap()
}

#[derive(Clone)]
struct MockState {
    base_url: Arc<str>,
    wav: Arc<Vec<u8>>,
    calls: Arc<AtomicUsize>,
    last_payload: Arc<Mutex<Option<Value>>>,
}

struct MockDashscope {
    endpoint: String,
    calls: Arc<AtomicUsize>,
    last_payload: Arc<Mutex<Option<Value>>>,
}

impl MockDashscope {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let last_payload = Arc::new(Mutex::new(None));
        let state = MockState {
            base_url: format!("http://{address}").into(),
            wav: Arc::new(sine_wave_wav()),
            calls: calls.clone(),
            last_payload: last_payload.clone(),
        };
        let app = Router::new()
            .route("/generate", post(mock_generate))
            .route("/customization", post(mock_customization))
            .route("/uploads", get(mock_upload_policy))
            .route("/oss", post(mock_oss_upload))
            .route("/audio.wav", get(mock_audio))
            .with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            endpoint: format!("http://{address}/generate"),
            calls,
            last_payload,
        }
    }
}

async fn mock_generate(State(state): State<MockState>, Json(payload): Json<Value>) -> Json<Value> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    *state.last_payload.lock().unwrap() = Some(payload.clone());
    let characters = payload
        .pointer("/input/text")
        .and_then(Value::as_str)
        .map_or(0, |text| text.chars().count());
    Json(json!({
        "status_code": 200,
        "request_id": "integration-test",
        "output": { "audio": { "url": format!("{}/audio.wav", state.base_url) } },
        "usage": { "characters": characters }
    }))
}

async fn mock_upload_policy(State(state): State<MockState>) -> Json<Value> {
    Json(json!({
        "data": {
            "policy": "policy",
            "signature": "signature",
            "upload_dir": "dashscope-instant/test",
            "upload_host": format!("{}/oss", state.base_url),
            "oss_access_key_id": "key",
            "x_oss_object_acl": "private",
            "x_oss_forbid_overwrite": "true",
        }
    }))
}

async fn mock_oss_upload(body: axum::body::Bytes) -> StatusCode {
    if body.len() > 1_000 {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    }
}

async fn mock_customization(Json(payload): Json<Value>) -> Json<Value> {
    use base64::Engine;
    let input = &payload["input"];
    match input["action"].as_str() {
        Some("create_voice")
            if input["url"]
                .as_str()
                .is_some_and(|url| url.starts_with("oss://")) =>
        {
            Json(json!({ "output": { "voice_id": "qwen-audio-3.1-tts-flash-ss13-clone" } }))
        }
        Some("query_voice") => Json(json!({ "output": { "status": "OK" } })),
        Some("create_voice") => Json(json!({
            "output": {
                "voice_id": "qwen-audio-3.1-tts-flash-vd-ss13-mock",
                "preview_audio": {
                    "data": base64::engine::general_purpose::STANDARD.encode(sine_wave_wav()),
                },
            },
        })),
        _ => Json(json!({ "output": {} })),
    }
}

async fn mock_audio(
    State(state): State<MockState>,
) -> ([(header::HeaderName, &'static str); 1], Vec<u8>) {
    (
        [(header::CONTENT_TYPE, "audio/wav")],
        state.wav.as_ref().clone(),
    )
}

fn sine_wave_wav() -> Vec<u8> {
    let sample_rate = 24_000_u32;
    let frames = sample_rate / 2;
    let data_size = frames * 2;
    let mut output = Vec::with_capacity(44 + data_size as usize);
    output.extend_from_slice(b"RIFF");
    output.extend_from_slice(&(36 + data_size).to_le_bytes());
    output.extend_from_slice(b"WAVEfmt ");
    output.extend_from_slice(&16_u32.to_le_bytes());
    output.extend_from_slice(&1_u16.to_le_bytes());
    output.extend_from_slice(&1_u16.to_le_bytes());
    output.extend_from_slice(&sample_rate.to_le_bytes());
    output.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    output.extend_from_slice(&2_u16.to_le_bytes());
    output.extend_from_slice(&16_u16.to_le_bytes());
    output.extend_from_slice(b"data");
    output.extend_from_slice(&data_size.to_le_bytes());
    for index in 0..frames {
        let sample = (TAU * 240.0 * index as f32 / sample_rate as f32).sin() * 0.2;
        output.extend_from_slice(&((sample * i16::MAX as f32) as i16).to_le_bytes());
    }
    output
}
