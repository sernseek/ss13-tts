use std::{
    borrow::Cow,
    collections::hash_map::DefaultHasher,
    f32::consts::TAU,
    hash::{Hash, Hasher},
    io::Cursor,
    num::{NonZeroU8, NonZeroU32},
};

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, errors::Error as DecodeError,
    formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions, probe::Hint,
};
use unicode_segmentation::UnicodeSegmentation;
use vorbis_rs::VorbisEncoderBuilder;

use crate::error::AppError;

#[derive(Clone, Debug)]
pub struct AudioData {
    pub sample_rate: u32,
    pub channels: Vec<Vec<f32>>,
}

impl AudioData {
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn duration_seconds(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }
}

fn normalize_streaming_wav_header(bytes: &[u8]) -> Cow<'_, [u8]> {
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Cow::Borrowed(bytes);
    }

    let mut position = 12_usize;
    let mut block_align = 1_usize;
    while position + 8 <= bytes.len() {
        let chunk_id = &bytes[position..position + 4];
        let declared_size = u32::from_le_bytes([
            bytes[position + 4],
            bytes[position + 5],
            bytes[position + 6],
            bytes[position + 7],
        ]) as usize;
        let data_start = position + 8;
        if chunk_id == b"fmt " && declared_size >= 14 && data_start + 14 <= bytes.len() {
            block_align = usize::from(u16::from_le_bytes([
                bytes[data_start + 12],
                bytes[data_start + 13],
            ]))
            .max(1);
        }
        if chunk_id == b"data" {
            let available_size = bytes.len().saturating_sub(data_start);
            if declared_size <= available_size && declared_size.is_multiple_of(block_align) {
                return Cow::Borrowed(bytes);
            }

            let actual_size = available_size - available_size % block_align;
            let final_length = data_start + actual_size;
            let Ok(actual_size_u32) = u32::try_from(actual_size) else {
                return Cow::Borrowed(bytes);
            };
            let Ok(riff_size_u32) = u32::try_from(final_length.saturating_sub(8)) else {
                return Cow::Borrowed(bytes);
            };
            let mut normalized = bytes[..final_length].to_vec();
            normalized[position + 4..position + 8].copy_from_slice(&actual_size_u32.to_le_bytes());
            normalized[4..8].copy_from_slice(&riff_size_u32.to_le_bytes());
            return Cow::Owned(normalized);
        }

        let padded_size = declared_size.saturating_add(declared_size & 1);
        let Some(next_position) = data_start.checked_add(padded_size) else {
            break;
        };
        if next_position > bytes.len() {
            break;
        }
        position = next_position;
    }
    Cow::Borrowed(bytes)
}

pub fn decode_wav(bytes: &[u8]) -> Result<AudioData, AppError> {
    let normalized = normalize_streaming_wav_header(bytes);
    let mut reader = WavReader::new(Cursor::new(normalized.as_ref()))
        .map_err(|error| AppError::Audio(format!("invalid WAV response: {error}")))?;
    let spec = reader.spec();
    let channel_count = usize::from(spec.channels);
    if channel_count == 0 || channel_count > 2 {
        return Err(AppError::Audio(format!(
            "unsupported WAV channel count: {}",
            spec.channels
        )));
    }
    if !(8_000..=96_000).contains(&spec.sample_rate) {
        return Err(AppError::Audio(format!(
            "unsupported WAV sample rate: {}",
            spec.sample_rate
        )));
    }

    let samples = match spec.sample_format {
        SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| sample.map(|value| value.clamp(-1.0, 1.0)))
            .collect::<Result<Vec<_>, _>>(),
        SampleFormat::Int if spec.bits_per_sample <= 16 => {
            let scale = ((1_i64 << (spec.bits_per_sample - 1)) - 1) as f32;
            reader
                .samples::<i16>()
                .map(|sample| sample.map(|value| (value as f32 / scale).clamp(-1.0, 1.0)))
                .collect::<Result<Vec<_>, _>>()
        }
        SampleFormat::Int => {
            let scale = ((1_i64 << (spec.bits_per_sample - 1)) - 1) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| (value as f32 / scale).clamp(-1.0, 1.0)))
                .collect::<Result<Vec<_>, _>>()
        }
    }
    .map_err(|error| AppError::Audio(format!("failed to decode WAV samples: {error}")))?;

    let mut channels = vec![Vec::with_capacity(samples.len() / channel_count); channel_count];
    for (index, sample) in samples.into_iter().enumerate() {
        channels[index % channel_count].push(sample);
    }
    let frames = channels[0].len();
    if frames == 0 || channels.iter().any(|channel| channel.len() != frames) {
        return Err(AppError::Audio(
            "WAV contains incomplete audio frames".into(),
        ));
    }

    Ok(AudioData {
        sample_rate: spec.sample_rate,
        channels,
    })
}

/// Decodes an uploaded recording in any common format (WAV, MP3, M4A/AAC, Ogg Vorbis, FLAC)
/// into mono audio, stopping once `max_seconds` have been read.
pub fn decode_recording(bytes: Vec<u8>, max_seconds: f64) -> Result<AudioData, AppError> {
    let invalid = |error: DecodeError| {
        AppError::BadRequest(format!(
            "无法读取音频文件（支持 WAV、MP3、M4A、OGG、FLAC）：{error}"
        ))
    };
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(invalid)?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| AppError::BadRequest("音频文件里没有音轨".into()))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(invalid)?;

    let mut sample_rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut mono = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(DecodeError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(DecodeError::ResetRequired) => break,
            Err(error) => return Err(invalid(error)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(DecodeError::DecodeError(_)) => continue,
            Err(error) => return Err(invalid(error)),
        };
        let spec = *decoded.spec();
        sample_rate = spec.rate;
        let channel_count = spec.channels.count().max(1);
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        for frame in buffer.samples().chunks(channel_count) {
            mono.push(frame.iter().sum::<f32>() / channel_count as f32);
        }
        if sample_rate > 0 && mono.len() as f64 >= max_seconds * f64::from(sample_rate) {
            mono.truncate((max_seconds * f64::from(sample_rate)) as usize);
            break;
        }
    }
    if sample_rate == 0 || mono.is_empty() {
        return Err(AppError::BadRequest("音频文件里没有声音".into()));
    }
    Ok(AudioData {
        sample_rate,
        channels: vec![mono],
    })
}

/// Encodes the first channel as 16-bit PCM WAV.
pub fn encode_wav(audio: &AudioData) -> Result<Vec<u8>, AppError> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: audio.sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut output = Cursor::new(Vec::new());
    let mut writer = WavWriter::new(&mut output, spec)
        .map_err(|error| AppError::Audio(format!("failed to start WAV: {error}")))?;
    for sample in audio.channels.first().into_iter().flatten() {
        writer
            .write_sample((sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .map_err(|error| AppError::Audio(format!("failed to write WAV: {error}")))?;
    }
    writer
        .finalize()
        .map_err(|error| AppError::Audio(format!("failed to finish WAV: {error}")))?;
    Ok(output.into_inner())
}

pub fn encode_ogg(audio: &AudioData) -> Result<Vec<u8>, AppError> {
    if audio.frames() == 0 || audio.channels.is_empty() {
        return Err(AppError::Audio("cannot encode empty audio".into()));
    }
    let sample_rate = NonZeroU32::new(audio.sample_rate)
        .ok_or_else(|| AppError::Audio("sample rate cannot be zero".into()))?;
    let channel_count = u8::try_from(audio.channels.len())
        .ok()
        .and_then(NonZeroU8::new)
        .ok_or_else(|| AppError::Audio("invalid channel count".into()))?;
    let mut builder =
        VorbisEncoderBuilder::new(sample_rate, channel_count, Vec::new()).map_err(|error| {
            AppError::Audio(format!("failed to initialize Vorbis encoder: {error}"))
        })?;
    let mut encoder = builder
        .build()
        .map_err(|error| AppError::Audio(format!("failed to build Vorbis encoder: {error}")))?;

    for start in (0..audio.frames()).step_by(8_192) {
        let end = (start + 8_192).min(audio.frames());
        let block: Vec<&[f32]> = audio
            .channels
            .iter()
            .map(|channel| &channel[start..end])
            .collect();
        encoder
            .encode_audio_block(&block)
            .map_err(|error| AppError::Audio(format!("failed to encode Vorbis audio: {error}")))?;
    }
    encoder
        .finish()
        .map_err(|error| AppError::Audio(format!("failed to finish Vorbis stream: {error}")))
}

pub fn apply_effects(
    source: &AudioData,
    legacy_filter: bool,
    silicon: bool,
    radio: bool,
    seed: &str,
) -> AudioData {
    let mut output = source.clone();
    if legacy_filter {
        for channel in &mut output.channels {
            band_limit(channel, output.sample_rate, 280.0, 3_600.0);
            for sample in channel {
                *sample = (*sample * 2.3).tanh() * 0.78;
            }
        }
    }
    if silicon {
        for channel in &mut output.channels {
            let delay = (output.sample_rate as f32 * 0.028) as usize;
            let original = channel.clone();
            for (index, sample) in channel.iter_mut().enumerate() {
                let ring = (TAU * 42.0 * index as f32 / output.sample_rate as f32).sin();
                let echo = index
                    .checked_sub(delay)
                    .map_or(0.0, |position| original[position] * 0.28);
                *sample = (original[index] * (0.72 + 0.22 * ring) + echo).clamp(-0.92, 0.92);
            }
        }
    }
    if radio {
        output = radio_effect(&output, seed);
    }
    output
}

pub fn make_blips(text: &str, voice: &str, blip_base: &str, blip_number: &str) -> AudioData {
    let sample_rate = 24_000_u32;
    let mut hasher = DefaultHasher::new();
    voice.hash(&mut hasher);
    blip_number.hash(&mut hasher);
    let voice_hash = hasher.finish();
    let base_frequency = if blip_base.eq_ignore_ascii_case("female") {
        280.0
    } else {
        175.0
    };
    let frequency = base_frequency + (voice_hash % 95) as f32;
    let mut samples = Vec::new();

    for grapheme in text.graphemes(true).take(160) {
        if grapheme.chars().all(char::is_whitespace) {
            append_silence(&mut samples, sample_rate, 0.035);
            continue;
        }
        if grapheme.chars().all(|character| {
            character.is_ascii_punctuation()
                || "，。！？；：、…—“”‘’（）《》【】".contains(character)
        }) {
            append_silence(&mut samples, sample_rate, 0.065);
            continue;
        }
        append_tone(&mut samples, sample_rate, frequency, 0.040, 0.22);
        append_silence(&mut samples, sample_rate, 0.018);
    }
    if samples.is_empty() {
        append_silence(&mut samples, sample_rate, 0.08);
    }

    AudioData {
        sample_rate,
        channels: vec![samples],
    }
}

fn radio_effect(source: &AudioData, seed: &str) -> AudioData {
    let prefix_frames = (source.sample_rate as f32 * 0.055) as usize;
    let suffix_frames = (source.sample_rate as f32 * 0.075) as usize;
    let mut state = seed_value(seed);
    let mut channels = Vec::with_capacity(source.channels.len());

    for input in &source.channels {
        let mut speech = input.clone();
        band_limit(&mut speech, source.sample_rate, 320.0, 3_100.0);
        for sample in &mut speech {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let noise = (((state >> 33) as u32 as f32 / u32::MAX as f32) - 0.5) * 0.022;
            *sample = ((*sample * 2.8).tanh() * 0.72 + noise).clamp(-0.9, 0.9);
            *sample = (*sample * 96.0).round() / 96.0;
        }

        let mut output = Vec::with_capacity(prefix_frames + speech.len() + suffix_frames);
        for index in 0..prefix_frames {
            let envelope = 1.0 - index as f32 / prefix_frames.max(1) as f32;
            output.push(
                (TAU * 1_150.0 * index as f32 / source.sample_rate as f32).sin() * 0.16 * envelope,
            );
        }
        output.extend(speech);
        for index in 0..suffix_frames {
            state = state
                .wrapping_mul(2_862_933_555_777_941_757)
                .wrapping_add(3_037_000_493);
            let noise = (((state >> 32) as u32 as f32 / u32::MAX as f32) - 0.5) * 0.12;
            let envelope = 1.0 - index as f32 / suffix_frames.max(1) as f32;
            output.push(noise * envelope);
        }
        channels.push(output);
    }

    AudioData {
        sample_rate: source.sample_rate,
        channels,
    }
}

fn band_limit(samples: &mut [f32], sample_rate: u32, low_cut: f32, high_cut: f32) {
    let dt = 1.0 / sample_rate as f32;
    let high_rc = 1.0 / (TAU * low_cut);
    let high_alpha = high_rc / (high_rc + dt);
    let low_rc = 1.0 / (TAU * high_cut);
    let low_alpha = dt / (low_rc + dt);
    let mut previous_input = 0.0;
    let mut previous_high = 0.0;
    let mut previous_low = 0.0;

    for sample in samples {
        let high = high_alpha * (previous_high + *sample - previous_input);
        let low = previous_low + low_alpha * (high - previous_low);
        previous_input = *sample;
        previous_high = high;
        previous_low = low;
        *sample = low;
    }
}

fn append_tone(
    samples: &mut Vec<f32>,
    sample_rate: u32,
    frequency: f32,
    seconds: f32,
    amplitude: f32,
) {
    let frames = (sample_rate as f32 * seconds) as usize;
    for index in 0..frames {
        let phase = index as f32 / frames.max(1) as f32;
        let envelope = (phase * std::f32::consts::PI).sin();
        samples.push(
            (TAU * frequency * index as f32 / sample_rate as f32).sin() * amplitude * envelope,
        );
    }
}

fn append_silence(samples: &mut Vec<f32>, sample_rate: u32, seconds: f32) {
    samples.resize(samples.len() + (sample_rate as f32 * seconds) as usize, 0.0);
}

fn seed_value(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recordings_round_trip_through_wav() {
        let audio = AudioData {
            sample_rate: 16_000,
            channels: vec![
                (0..32_000)
                    .map(|index| (index as f32 / 20.0).sin() * 0.5)
                    .collect(),
            ],
        };
        let wav = encode_wav(&audio).unwrap();
        let decoded = decode_recording(wav, 1.5).unwrap();
        assert_eq!(decoded.sample_rate, 16_000);
        assert_eq!(
            decoded.frames(),
            24_000,
            "decoding should stop at the time limit"
        );
        assert!(decode_recording(b"not audio at all".to_vec(), 60.0).is_err());
    }

    #[test]
    fn chinese_blips_are_real_vorbis_audio() {
        let audio = make_blips("你好，空间站！", "Cherry Woman", "female", "1");
        let encoded = encode_ogg(&audio).expect("encode Ogg");
        assert!(encoded.starts_with(b"OggS"));
        assert!(audio.duration_seconds() > 0.2);
    }

    #[test]
    fn decodes_dashscope_streaming_wav_sizes() {
        let samples = [0_i16, 1_000, -1_000, 0];
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&0x7fff_ffbf_u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&24_000_u32.to_le_bytes());
        wav.extend_from_slice(&48_000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0x7fff_ffdb_u32.to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }

        let decoded = decode_wav(&wav).expect("decode streaming WAV");
        assert_eq!(decoded.sample_rate, 24_000);
        assert_eq!(decoded.channels.len(), 1);
        assert_eq!(decoded.frames(), samples.len());
    }
}
