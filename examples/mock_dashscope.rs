use std::{f32::consts::TAU, net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderValue, header},
    response::Response,
    routing::{get, post},
};
use serde_json::{Value, json};

#[derive(Clone)]
struct MockState {
    base_url: Arc<str>,
    wav: Arc<Vec<u8>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr: SocketAddr = std::env::var("MOCK_BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:5010".into())
        .parse()?;
    let state = MockState {
        base_url: format!("http://{bind_addr}").into(),
        wav: Arc::new(sine_wave_wav()),
    };
    let app = Router::new()
        .route(
            "/api/v1/services/aigc/multimodal-generation/generation",
            post(generate),
        )
        .route(
            "/api/v1/services/audio/tts/SpeechSynthesizer",
            post(generate),
        )
        .route("/audio.wav", get(audio))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    println!("Mock DashScope ready on http://{bind_addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn generate(State(state): State<MockState>, Json(payload): Json<Value>) -> Json<Value> {
    let characters = payload
        .pointer("/input/text")
        .and_then(Value::as_str)
        .map_or(0, |text| text.chars().count());
    Json(json!({
        "status_code": 200,
        "request_id": "local-mock-request",
        "code": "",
        "message": "",
        "output": {
            "audio": {
                "url": format!("{}/audio.wav", state.base_url),
                "data": "",
                "id": "audio_local_mock"
            }
        },
        "usage": { "characters": characters }
    }))
}

async fn audio(State(state): State<MockState>) -> Response {
    let mut response = Response::new(Body::from(state.wav.as_ref().clone()));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
    response
}

fn sine_wave_wav() -> Vec<u8> {
    let sample_rate = 24_000_u32;
    let frames = sample_rate * 6 / 5;
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
        let time = index as f32 / sample_rate as f32;
        let envelope = (1.0 - time / 1.2).clamp(0.0, 1.0);
        let sample = (TAU * 220.0 * time).sin() * 0.24 * envelope;
        output.extend_from_slice(&((sample * i16::MAX as f32) as i16).to_le_bytes());
    }
    output
}
