use base64::{Engine, engine::general_purpose::STANDARD};
use qwen3_tts::{Language, Qwen3TTS, Speaker, SynthesisOptions, auto_device};
use rodio::{OutputStreamBuilder, Sink, Source, buffer::SamplesBuffer};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

fn emit(event: Value) {
    let mut output = io::stdout().lock();
    let _ = writeln!(output, "{event}");
    let _ = output.flush();
}

#[derive(Default)]
struct SpeechLevel {
    previous_gain: Option<f32>,
}

impl SpeechLevel {
    fn apply(&mut self, samples: &mut [f32], rate: u32) {
        let window = (rate as usize / 50).max(1);
        let blend = (window / 5).max(1);
        for frame in samples.chunks_mut(window) {
            let mean_power =
                frame.iter().map(|sample| sample * sample).sum::<f32>() / frame.len() as f32;
            let gain = 3.4
                * (0.065 / mean_power.sqrt().max(0.015))
                    .powf(0.45)
                    .clamp(0.65, 1.30);
            let previous = self.previous_gain.replace(gain).unwrap_or(gain);
            // Measure the current window. Carry the prior gain only across the boundary.
            for (index, sample) in frame.iter_mut().enumerate() {
                let weight = ((index + 1) as f32 / blend as f32).min(1.0);
                *sample = soften_peak(
                    *sample * (previous + weight * (gain - previous)),
                    0.50,
                    0.90,
                );
            }
        }
    }
}

fn soften_peak(sample: f32, knee: f32, ceiling: f32) -> f32 {
    let excess = sample.abs() - knee;
    if excess > 0.0 {
        (knee + excess / (1.0 + excess / (ceiling - knee))).copysign(sample)
    } else {
        sample
    }
}

// Skip small stretches of the waveform between aligned, cross-faded windows.
fn speed_up_web_speech(samples: &[f32], rate: u32) -> Vec<f32> {
    const SPEED: f64 = 1.15;
    let overlap = rate as usize * 8 / 1000;
    let hop = rate as usize * 16 / 1000;
    let search = rate as usize * 3 / 1000;
    let length = (samples.len() as f64 / SPEED).round() as usize;
    if overlap == 0 || length <= hop + overlap {
        return samples.to_vec();
    }

    let mut output = Vec::with_capacity(length);
    output.extend_from_slice(&samples[..hop + overlap]);
    for offset in (hop..length).step_by(hop) {
        let width = (hop + overlap).min(length - offset);
        let fade = overlap.min(width);
        let expected = (offset as f64 * SPEED).round() as usize;
        let limit = samples.len() - width;
        let start = if offset + width == length {
            limit
        } else {
            let center = expected.min(limit);
            let mut best = (center, f32::NEG_INFINITY);
            for candidate in center.saturating_sub(search)..=(center + search).min(limit) {
                let mut agreement = 0.0;
                let mut power = 0.0;
                for i in (0..fade).step_by(2) {
                    let sample = samples[candidate + i];
                    agreement += sample * output[offset + i];
                    power += sample * sample;
                }
                let score = agreement / (power + 1e-12_f32).sqrt();
                if score > best.1
                    || (score == best.1 && candidate.abs_diff(center) < best.0.abs_diff(center))
                {
                    best = (candidate, score);
                }
            }
            best.0
        };
        for i in 0..fade {
            let blend = (i + 1) as f32 / (fade + 1) as f32;
            output[offset + i] = output[offset + i] * (1.0 - blend) + samples[start + i] * blend;
        }
        output.extend_from_slice(&samples[start + fade..start + width]);
        if output.len() == length {
            break;
        }
    }
    output
}

// Carry the silence limit across packets from the same synthesized phrase.
#[derive(Default)]
struct SpeechPauses {
    heard_speech: bool,
    quiet_samples: usize,
}

impl SpeechPauses {
    fn shorten(&mut self, samples: &[f32], rate: u32) -> Vec<f32> {
        let window = (rate as usize / 100).max(1);
        let pause_limit = rate as usize * 120 / 1000;
        let mut output = Vec::with_capacity(samples.len());
        for frame in samples.chunks(window) {
            let power = frame.iter().map(|sample| sample * sample).sum::<f32>();
            let quiet = power < frame.len() as f32 * 0.007_f32.powi(2)
                && frame.iter().all(|sample| sample.abs() < 0.03);
            if quiet {
                if self.heard_speech {
                    let keep = frame
                        .len()
                        .min(pause_limit.saturating_sub(self.quiet_samples));
                    output.extend_from_slice(&frame[..keep]);
                    self.quiet_samples += keep;
                }
            } else {
                self.heard_speech = true;
                self.quiet_samples = 0;
                output.extend_from_slice(frame);
            }
        }
        output
    }
}

struct PlaybackMeter {
    samples: SamplesBuffer,
    id: u64,
    levels: mpsc::SyncSender<(u64, f32)>,
    volume: Arc<AtomicU16>,
    gain: Option<f32>,
    window: usize,
    count: usize,
    power: f32,
}

impl Iterator for PlaybackMeter {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = self.samples.next()?;
        let target = f32::from(self.volume.load(Ordering::Relaxed)) / 100.0;
        let gain = self.gain.get_or_insert(target);
        let change = target - *gain;
        *gain += if change.abs() < 0.001 {
            change
        } else {
            change * 0.005
        };
        let sample = soften_peak(sample * *gain, 0.89, 0.98);
        self.power += sample * sample;
        self.count += 1;
        if self.count == self.window {
            let rms = (self.power / self.count as f32).sqrt();
            let _ = self.levels.try_send((self.id, rms));
            self.power = 0.0;
            self.count = 0;
        }
        Some(sample)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.samples.size_hint()
    }
}

impl Source for PlaybackMeter {
    fn current_span_len(&self) -> Option<usize> {
        self.samples.current_span_len()
    }
    fn channels(&self) -> u16 {
        self.samples.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.samples.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.samples.total_duration()
    }
}

fn main() {
    if let Err(error) = serve() {
        emit(json!({"event":"fatal", "message":format!("{error:#}")}));
        std::process::exit(1);
    }
}

fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let directory = args.next().ok_or("model directory missing")?;
    let remote = args.next().as_deref() == Some("--pcm");
    let output = (!remote)
        .then(OutputStreamBuilder::open_default_stream)
        .transpose()?;
    let serial = Arc::new(AtomicU64::new(0));
    let volume = Arc::new(AtomicU16::new(100));
    let meter = output.as_ref().map(|_| {
        let (levels, updates) = mpsc::sync_channel(1);
        let playback_serial = serial.clone();
        thread::spawn(move || {
            for (id, level) in updates {
                if playback_serial.load(Ordering::Acquire) == id {
                    emit(json!({"id":id, "event":"level", "rms":level}));
                }
            }
        });
        levels
    });
    let playback: Arc<Mutex<Option<Arc<Sink>>>> = Arc::new(Mutex::new(None));
    let (requests, queue) = mpsc::channel::<(u64, String, Language)>();
    let input_serial = serial.clone();
    let input_volume = volume.clone();
    let input_playback = playback.clone();
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(volume) = message["volume"].as_u64() {
                input_volume.store(volume.min(200) as u16, Ordering::Relaxed);
            }
            let Some(id) = message["id"].as_u64() else {
                continue;
            };
            input_serial.store(id, Ordering::Release);
            if let Some(sink) = input_playback.lock().expect("player mutex").take() {
                sink.stop();
            }
            if id != 0
                && let Some(text) = message["text"].as_str()
                && requests
                    .send((
                        id,
                        text.to_owned(),
                        message["language"]
                            .as_str()
                            .and_then(|code| code.parse::<Language>().ok())
                            .unwrap_or(Language::Korean),
                    ))
                    .is_err()
            {
                break;
            }
        }
        input_serial.store(0, Ordering::Release);
        if let Some(sink) = input_playback.lock().expect("player mutex").take() {
            sink.stop();
        }
    });

    let load = Instant::now();
    let device = auto_device()?;
    let model = Qwen3TTS::from_pretrained(&directory, device)?;
    let load_ms = load.elapsed().as_millis();
    let prepare = Instant::now();
    // Build the first Metal kernels while the caller is still speaking, without playing a sample.
    // A request queued during model loading takes priority over this warm-up.
    if serial.load(Ordering::Acquire) == 0 {
        let options = SynthesisOptions {
            chunk_frames: 6,
            max_length: 12,
            ..Default::default()
        };
        if let Ok(mut stream) =
            model.synthesize_streaming("안녕하세요.", Speaker::Sohee, Language::Korean, options)
        {
            let _ = stream.next();
        }
    }
    emit(json!({"event":"ready", "load_ms":load_ms, "prepare_ms":prepare.elapsed().as_millis()}));
    while let Ok((id, text, language)) = queue.recv_timeout(Duration::from_secs(180)) {
        if serial.load(Ordering::Acquire) != id {
            continue;
        }
        let sink = output
            .as_ref()
            .map(|output| Arc::new(Sink::connect_new(output.mixer())));
        {
            let mut active = playback.lock().expect("player mutex");
            if serial.load(Ordering::Acquire) != id {
                continue;
            }
            *active = sink.clone();
        }
        let start = Instant::now();
        let mut first = true;
        let mut samples = 0usize;
        let mut pauses = SpeechPauses::default();
        let mut level = SpeechLevel::default();
        let mut sample_rate = 24000;
        let max_length = (text.chars().count() * 14 + 48).clamp(72, 1300);
        let speaker = match language {
            Language::Korean => Speaker::Sohee,
            _ => Speaker::Ryan,
        };
        let result = model.synthesize_streaming(&text, speaker, language,
            SynthesisOptions { chunk_frames: 6, max_length, ..Default::default() })
            .and_then(|stream| {
                for buffer in stream {
                    if serial.load(Ordering::Acquire) != id { break; }
                    let buffer = buffer?;
                    if buffer.samples.is_empty() { continue; }
                    let _active = playback.lock().expect("player mutex");
                    if serial.load(Ordering::Acquire) != id { break; }
                    sample_rate = buffer.sample_rate;
                    let audio = if sink.is_some() {
                        buffer.samples
                    } else {
                        speed_up_web_speech(&buffer.samples, sample_rate)
                    };
                    let mut audio = pauses.shorten(&audio, sample_rate);
                    if audio.is_empty() { continue; }
                    level.apply(&mut audio, sample_rate);
                    samples += audio.len();
                    if let Some((sink, levels)) = sink.as_ref().zip(meter.as_ref()) {
                        sink.append(PlaybackMeter {
                            samples: SamplesBuffer::new(1, sample_rate, audio),
                            id,
                            levels: levels.clone(),
                            volume: volume.clone(),
                            gain: None,
                            window: (sample_rate as usize / 25).max(1),
                            count: 0,
                            power: 0.0,
                        });
                    } else {
                        let packet: Vec<u8> = audio.into_iter()
                            .flat_map(|sample| ((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
                            .collect();
                        emit(json!({"id":id, "event":"pcm", "rate":sample_rate,"data":STANDARD.encode(packet)}));
                    }
                    if first {
                        emit(json!({"id":id, "event":"playing", "first_ms":start.elapsed().as_millis()}));
                        first = false;
                    }
                }
                Ok(())
            });
        let synthesis_ms = start.elapsed().as_millis();
        if serial.load(Ordering::Acquire) != id {
            continue;
        }
        match result {
            Ok(()) if !first => {
                while sink.as_ref().is_some_and(|sink| !sink.empty())
                    && serial.load(Ordering::Acquire) == id
                {
                    thread::sleep(Duration::from_millis(30));
                }
                if serial.load(Ordering::Acquire) == id {
                    emit(
                        json!({"id":id,"event":"finished","synthesis_ms":synthesis_ms,
                        "audio_ms":samples as u64 * 1000 / u64::from(sample_rate)}),
                    );
                }
            }
            Ok(()) => emit(json!({"id":id,"event":"error","message":"생성된 음성이 없어."})),
            Err(error) => emit(json!({"id":id,"event":"error","message":format!("{error:#}")})),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PlaybackMeter, SpeechLevel, SpeechPauses, speed_up_web_speech};
    use rodio::buffer::SamplesBuffer;
    use std::sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
        mpsc,
    };

    #[test]
    fn mac_volume_applies_to_queued_audio_and_can_mute_or_boost_it() {
        let volume = Arc::new(AtomicU16::new(0));
        let (levels, _) = mpsc::sync_channel(1);
        let mut playback = PlaybackMeter {
            samples: SamplesBuffer::new(1, 24_000, vec![0.65; 2000]),
            id: 1,
            levels,
            volume: volume.clone(),
            gain: None,
            window: 960,
            count: 0,
            power: 0.0,
        };
        assert_eq!(playback.next(), Some(0.0));
        volume.store(200, Ordering::Relaxed);
        let loud = playback.nth(1200).unwrap();
        assert!(loud > 0.8 && loud < 0.98, "{loud}");
    }

    #[test]
    fn web_playback_shortens_audio_while_preserving_pitch() {
        let rate = 24_000;
        let samples: Vec<_> = (0..rate)
            .map(|i| (std::f32::consts::TAU * 240.0 * i as f32 / rate as f32).sin())
            .collect();
        let faster = speed_up_web_speech(&samples, rate);
        assert_eq!(faster.len(), 20_870);
        assert_eq!(faster.last(), samples.last());
        let cycles = faster
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count();
        let fundamental = cycles as f64 * f64::from(rate) / faster.len() as f64;
        assert!((fundamental - 240.0).abs() < 5.0, "{fundamental}");
        assert_eq!(speed_up_web_speech(&[0.1; 50], rate), vec![0.1; 50]);
    }

    #[test]
    fn long_pauses_are_limited_across_packets_and_short_pauses_are_kept() {
        let rate = 1000;
        let mut pauses = SpeechPauses::default();
        assert!(pauses.shorten(&[0.0; 250], rate).is_empty());

        let mut packet = vec![0.3; 50];
        packet.extend([0.0; 100]);
        assert_eq!(pauses.shorten(&packet, rate), packet);
        assert_eq!(pauses.shorten(&[0.0; 300], rate), vec![0.0; 20]);

        let mut next = vec![0.3; 50];
        next.extend([0.0; 100]);
        next.extend([0.3; 50]);
        assert_eq!(pauses.shorten(&next, rate), next);
    }

    #[test]
    fn speech_level_stays_the_same_after_a_loud_passage() {
        let rate = 24_000;
        let mut quiet = [0.035; 1440];
        let mut loud = [0.75; 12_000];
        let mut quiet_after_loud = quiet;
        let mut level = SpeechLevel::default();
        level.apply(&mut quiet, rate);
        level.apply(&mut loud, rate);
        level.apply(&mut quiet_after_loud, rate);
        assert_eq!(&quiet[96..], &quiet_after_loud[96..]);
        assert!(quiet[0] > 0.10);
        assert!(loud.iter().all(|sample| sample.abs() < 0.90));
    }
}
