//! Offline probe: compare the radio-processed clip against the untouched one for a real
//! provider WAV, so "is the radio effect actually audible" is a measurement, not a guess.
//! Usage: cargo run --release --example radio_probe -- <file.wav>...
use ss13_tts::audio::{apply_effects, decode_wav};

fn rms(channel: &[f32]) -> f32 {
    if channel.is_empty() {
        return 0.0;
    }
    (channel.iter().map(|s| s * s).sum::<f32>() / channel.len() as f32).sqrt()
}

/// Crude high-band energy estimate: RMS of the first difference rises with treble content,
/// so band-limiting to 3.1kHz should drop it noticeably.
fn high_band(channel: &[f32]) -> f32 {
    if channel.len() < 2 {
        return 0.0;
    }
    let diffs: Vec<f32> = channel.windows(2).map(|w| w[1] - w[0]).collect();
    rms(&diffs)
}

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).expect("read wav");
        let source = decode_wav(&bytes).expect("decode wav");
        let plain = apply_effects(&source, false, false, false, "probe");
        let radio = apply_effects(&source, false, false, true, "probe");
        let src = &source.channels[0];
        let p = &plain.channels[0];
        let r = &radio.channels[0];
        println!("{path}");
        println!(
            "  frames  source={} plain={} radio={} (radio adds {:.0}ms of lead-in/tail)",
            source.frames(),
            plain.frames(),
            radio.frames(),
            (radio.frames() as f64 - plain.frames() as f64) / source.sample_rate as f64 * 1000.0
        );
        println!(
            "  rms       source={:.4} radio={:.4}  ({:+.1}%)",
            rms(src),
            rms(r),
            (rms(r) / rms(src).max(f32::EPSILON) - 1.0) * 100.0
        );
        println!(
            "  high-band source={:.4} radio={:.4}  ({:+.1}%)",
            high_band(src),
            high_band(r),
            (high_band(r) / high_band(src).max(f32::EPSILON) - 1.0) * 100.0
        );
        let identical = p.len() == r.len() && p.iter().zip(r).all(|(a, b)| (a - b).abs() < 1e-6);
        println!("  radio output identical to plain: {identical}");
    }
}
