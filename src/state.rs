use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Semaphore};
use tracing::{info, warn};

use crate::{
    audio::{AudioData, decode_wav},
    config::Config,
    custom_voices::CustomVoices,
    error::AppError,
    provider::{DashscopeClient, Synthesis},
    voices::{Voice, VoiceCatalog},
};

pub struct AppState {
    pub config: Config,
    pub voices: VoiceCatalog,
    /// Player-made voices; only qwen-audio models support them.
    pub custom_voices: Option<Arc<CustomVoices>>,
    provider: DashscopeClient,
    provider_slots: Semaphore,
    jobs: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl AppState {
    pub async fn new(config: Config) -> Result<Arc<Self>, AppError> {
        prepare_cache_dir(&config.cache_dir).await?;
        let provider = DashscopeClient::new(&config)?;
        let voices = VoiceCatalog::new(config.backend, config.custom_voices.as_deref())
            .map_err(AppError::Internal)?;
        let custom_voices = if config.backend.supports_expression() {
            Some(Arc::new(
                CustomVoices::new(&config, provider.clone()).await?,
            ))
        } else {
            None
        };
        let state = Arc::new(Self {
            voices,
            custom_voices,
            provider_slots: Semaphore::new(config.provider_concurrency),
            provider,
            jobs: Mutex::new(HashMap::new()),
            config,
        });
        prune_cache(
            state.config.cache_dir.clone(),
            state.config.cache_max_bytes,
            state.config.cache_ttl,
        )
        .await;
        Ok(state)
    }

    /// Resolves a built-in, retired or approved player-made voice.
    pub async fn voice(&self, display: &str) -> Option<Voice> {
        if let Some(voice) = self.voices.find(display) {
            return Some(voice.clone());
        }
        self.custom_voices.as_ref()?.voice(display).await
    }

    pub async fn base_audio(&self, synthesis: &Synthesis<'_>) -> Result<Arc<AudioData>, AppError> {
        let request = self.provider.request_body(synthesis);
        let key = content_key(&request);
        let job = {
            let mut jobs = self.jobs.lock().await;
            jobs.entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let guard = job.lock().await;
        let result = self.load_or_synthesize(&key, &request).await;
        drop(guard);
        self.jobs.lock().await.remove(&key);
        result.map(Arc::new)
    }

    async fn load_or_synthesize(
        &self,
        key: &str,
        request: &serde_json::Value,
    ) -> Result<AudioData, AppError> {
        let cache_path = self.cache_path(key);
        if let Some(audio) = load_cache_entry(&cache_path, self.config.cache_ttl).await {
            info!(cache_key = key, "TTS cache hit");
            return Ok(audio);
        }

        let _permit = self
            .provider_slots
            .acquire()
            .await
            .map_err(|_| AppError::Internal("provider semaphore closed".into()))?;
        let wav = self.provider.synthesize(request).await?;
        let audio = decode_wav(&wav)?;
        atomic_write(&cache_path, &wav).await?;

        let cache_dir = self.config.cache_dir.clone();
        let max_bytes = self.config.cache_max_bytes;
        let ttl = self.config.cache_ttl;
        tokio::spawn(async move { prune_cache(cache_dir, max_bytes, ttl).await });
        Ok(audio)
    }

    fn cache_path(&self, key: &str) -> PathBuf {
        self.config.cache_dir.join(format!("{key}.wav"))
    }
}

/// The provider request fully determines the audio, so it doubles as the cache key.
fn content_key(request: &serde_json::Value) -> String {
    hex::encode(Sha256::digest(request.to_string().as_bytes()))
}

async fn prepare_cache_dir(path: &Path) -> Result<(), AppError> {
    tokio::fs::create_dir_all(path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}

async fn load_cache_entry(path: &Path, ttl: Duration) -> Option<AudioData> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    let age = SystemTime::now()
        .duration_since(metadata.modified().ok()?)
        .unwrap_or_default();
    if age > ttl {
        let _ = tokio::fs::remove_file(path).await;
        return None;
    }
    let bytes = tokio::fs::read(path).await.ok()?;
    match decode_wav(&bytes) {
        Ok(audio) => Some(audio),
        Err(error) => {
            warn!(path = %path.display(), %error, "removing invalid cache entry");
            let _ = tokio::fs::remove_file(path).await;
            None
        }
    }
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("tmp-{}-{timestamp}", std::process::id()));
    tokio::fs::write(&temporary, bytes).await?;
    tokio::fs::rename(&temporary, path).await?;
    Ok(())
}

async fn prune_cache(directory: PathBuf, max_bytes: u64, ttl: Duration) {
    let mut entries = Vec::new();
    let Ok(mut reader) = tokio::fs::read_dir(&directory).await else {
        return;
    };
    while let Ok(Some(entry)) = reader.next_entry().await {
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        let expired = SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default()
            > ttl;
        if expired {
            let _ = tokio::fs::remove_file(entry.path()).await;
            continue;
        }
        entries.push((modified, metadata.len(), entry.path()));
    }

    let mut total: u64 = entries.iter().map(|(_, size, _)| *size).sum();
    if total <= max_bytes {
        return;
    }
    entries.sort_by_key(|(modified, _, _)| *modified);
    for (_, size, path) in entries {
        if total <= max_bytes {
            break;
        }
        if tokio::fs::remove_file(path).await.is_ok() {
            total = total.saturating_sub(size);
        }
    }
}
