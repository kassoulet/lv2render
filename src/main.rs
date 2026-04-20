use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use hound::WavWriter;
use livi::{
    EmptyPortConnections, FeaturesBuilder, Instance, Plugin, PortType, WorkerManager,
    World, event::LV2AtomSequence,
};
use std::cmp::min;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::Arc;
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;
use symphonia::core::errors::Error as SymphoniaError;

#[derive(Debug, Clone)]
struct PluginSetting {
    plugin_identifier: String,
    params: Vec<(String, f32)>,
}

fn parse_plugin_setting(s: &str) -> Result<PluginSetting, String> {
    let parts: Vec<&str> = s.split(':').collect();
    let plugin_identifier = parts[0].to_string();
    let mut params = Vec::new();
    for i in 1..parts.len() {
        let param_part = parts[i];
        let kv: Vec<&str> = param_part.split('=').collect();
        if kv.len() == 2 {
            let value = kv[1]
                .parse::<f32>()
                .map_err(|_| format!("Invalid numeric value: {}", kv[1]))?;
            params.push((kv[0].to_string(), value));
        }
    }
    Ok(PluginSetting {
        plugin_identifier,
        params,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Input audio file path
    #[arg(short, long)]
    input: PathBuf,

    /// Output processed WAV file path
    #[arg(short, long)]
    output: PathBuf,

    /// List of plugins and parameters (e.g. "plugin_name:param=val")
    #[arg(trailing_var_arg = true, value_parser = parse_plugin_setting)]
    plugins: Vec<PluginSetting>,

    /// Number of samples per processing cycle
    #[arg(long, default_value_t = 1024)]
    block_size: u32,

    /// Seconds of silence to drain after input EOF (for reverb/delay tails)
    #[arg(long, default_value_t = 2.0)]
    drain_seconds: f64,
}

struct PluginInstance {
    plugin: Plugin,
    instance: Instance,
    control_inputs: Vec<f32>,
    control_outputs: Vec<f32>,
    atom_sequence_inputs: Vec<LV2AtomSequence>,
    atom_sequence_outputs: Vec<LV2AtomSequence>,
    audio_outputs: Vec<Vec<f32>>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Initializing LV2 world...");
    let world = World::new();

    if args.plugins.is_empty() {
        bail!("At least one plugin must be specified");
    }

    // Phase A: Input Probing
    println!("Probing input file: {:?}", args.input);
    let (mut format, track, sample_rate, num_channels, total_frames) =
        setup_input_audio(&args.input)?;

    println!(
        "Audio specs: SampleRate={}, Channels={}, Total frames={}",
        sample_rate, num_channels, total_frames
    );

    // Phase B: Plugin Chain Initialization
    let block_size = args.block_size as usize;
    let worker_manager = Arc::new(WorkerManager::default());
    let features = world.build_features(FeaturesBuilder {
        min_block_length: block_size,
        max_block_length: block_size,
        worker_manager: worker_manager.clone(),
    });

    let mut instances = Vec::new();
    let mut current_channels = num_channels;

    for (i, p_setting) in args.plugins.iter().enumerate() {
        let plugin = find_plugin(&world, &p_setting.plugin_identifier)
            .with_context(|| format!("Plugin '{}' not found", p_setting.plugin_identifier))?;

        println!("Adding plugin to chain: {} ({})", plugin.name(), plugin.uri());

        let port_counts = plugin.port_counts();
        if i == 0 {
            // Check compatibility with input audio
            if num_channels != port_counts.audio_inputs {
                if port_counts.audio_inputs == 2 && num_channels == 1 {
                    println!("Note: Upmixing mono input to stereo for first plugin");
                } else {
                    bail!("Channel mismatch: input has {} channels, first plugin expects {}", num_channels, port_counts.audio_inputs);
                }
            }
        } else {
            // Check compatibility with previous plugin
            if current_channels != port_counts.audio_inputs {
                bail!("Chain mismatch: plugin {} outputs {} channels, but plugin {} expects {}", i, current_channels, i+1, port_counts.audio_inputs);
            }
        }

        let mut instance = unsafe {
            plugin
                .instantiate(features.clone(), sample_rate as f64)
                .map_err(|e| anyhow!("Failed to instantiate plugin: {:?}", e))?
        };

        let mut control_inputs: Vec<f32> = plugin
            .ports_with_type(PortType::ControlInput)
            .map(|p| p.default_value)
            .collect();
        apply_parameter_settings(&plugin, &mut control_inputs, &p_setting.params)?;

        let control_outputs = vec![0.0f32; port_counts.control_outputs];
        let atom_sequence_inputs = (0..port_counts.atom_sequence_inputs)
            .map(|_| LV2AtomSequence::new(&features, 1024))
            .collect();
        let atom_sequence_outputs = (0..port_counts.atom_sequence_outputs)
            .map(|_| LV2AtomSequence::new(&features, 1024))
            .collect();
        let audio_outputs = vec![vec![0.0f32; block_size]; port_counts.audio_outputs];

        current_channels = port_counts.audio_outputs;
        instances.push(PluginInstance {
            plugin,
            instance,
            control_inputs,
            control_outputs,
            atom_sequence_inputs,
            atom_sequence_outputs,
            audio_outputs,
        });
    }

    // Phase C: Output Setup
    let wav_spec = hound::WavSpec {
        channels: current_channels as u16,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let file = std::fs::File::create(&args.output)
        .with_context(|| format!("Failed to create output file: {:?}", args.output))?;
    let writer = WavWriter::new(BufWriter::new(file), wav_spec)
        .with_context(|| format!("Failed to create WAV writer"))?;

    // Phase D: Processing
    println!("Starting processing loop with {} plugins...", instances.len());
    process_audio(
        format.as_mut(),
        track,
        &mut instances,
        writer,
        block_size,
        num_channels,
        &worker_manager,
        &features,
        args.drain_seconds,
    )?;

    println!("Processing complete. Output written to: {:?}", args.output);
    Ok(())
}

fn find_plugin(world: &World, identifier: &str) -> Option<Plugin> {
    if let Some(plugin) = world.plugin_by_uri(identifier) {
        return Some(plugin);
    }
    let matches: Vec<_> = world.iter_plugins()
        .filter(|p| p.name().to_lowercase().contains(&identifier.to_lowercase()))
        .collect();
    if matches.len() == 1 { Some(matches[0].clone()) } else { None }
}

fn apply_parameter_settings(plugin: &Plugin, control_inputs: &mut [f32], settings: &[(String, f32)]) -> Result<()> {
    let ports: Vec<_> = plugin.ports_with_type(PortType::ControlInput).collect();
    for (param, value) in settings {
        let port_idx = ports.iter().position(|p| p.name.to_lowercase() == param.to_lowercase());
        if let Some(idx) = port_idx {
            control_inputs[idx] = *value;
            println!("  Set parameter '{}' to {}", param, value);
        } else {
            bail!("Parameter '{}' not found in plugin '{}'", param, plugin.name());
        }
    }
    Ok(())
}

fn setup_input_audio(input_path: &PathBuf) -> Result<(Box<dyn symphonia::core::formats::FormatReader>, symphonia::core::formats::Track, u32, usize, u64)> {
    let file = std::fs::File::open(input_path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = input_path.extension() { hint.with_extension(&ext.to_string_lossy()); }
    let probe = symphonia::default::get_probe();
    let result = probe.format(&hint, mss, &FormatOptions::default(), &Default::default())?;
    let format = result.format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL).context("No audio")?.clone();
    let sample_rate = track.codec_params.sample_rate.context("No sample rate")?;
    let num_channels = track.codec_params.channels.map(|ch| ch.count()).context("No channels")?;
    Ok((format, track, sample_rate, num_channels, track.codec_params.n_frames.unwrap_or(0)))
}

fn process_audio(
    format: &mut dyn symphonia::core::formats::FormatReader,
    track: symphonia::core::formats::Track,
    instances: &mut [PluginInstance],
    mut writer: WavWriter<BufWriter<std::fs::File>>,
    block_size: usize,
    input_channels: usize,
    worker_manager: &Arc<WorkerManager>,
    features: &Arc<livi::Features>,
    drain_seconds: f64,
) -> Result<()> {
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let total_frames = track.codec_params.n_frames.unwrap_or(0);
    
    let first_plugin_input_count = instances[0].plugin.port_counts().audio_inputs;
    let mut input_buffer = vec![0.0f32; block_size * first_plugin_input_count];
    let needs_upmix = input_channels == 1 && first_plugin_input_count == 2;

    let drain_frames = (drain_seconds * sample_rate as f64) as usize;
    let mut drain_remaining = drain_frames;
    let mut is_draining = false;
    let mut frames_processed = 0u64;
    let mut frames_decoded = 0u64;
    let start_time = std::time::Instant::now();
    let mut decode_buffer: Option<Vec<Vec<f32>>> = None;

    loop {
        let mut current_frame_count = 0;
        if is_draining {
            if drain_remaining == 0 { break; }
            let chunk = min(block_size, drain_remaining);
            current_frame_count = chunk;
            let buf = decode_buffer.get_or_insert_with(|| vec![vec![0.0f32; block_size]; input_channels]);
            for ch in 0..input_channels { buf[ch].resize(chunk, 0.0); buf[ch].fill(0.0); }
            drain_remaining = drain_remaining.saturating_sub(chunk);
        } else {
            match format.next_packet() {
                Ok(packet) => match decoder.decode(&packet) {
                    Ok(decoded) => {
                        let num_frames = decoded.frames();
                        let num_chans = decoded.spec().channels.count();
                        let buf = decode_buffer.get_or_insert_with(|| vec![vec![0.0f32; 0]; num_chans]);
                        for ch in 0..num_chans { buf[ch].resize(num_frames, 0.0); }
                        copy_to_f32_buffer(decoded, buf);
                        current_frame_count = num_frames;
                        frames_decoded += num_frames as u64;
                    }
                    Err(_) => continue,
                },
                Err(SymphoniaError::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    is_draining = true;
                    eprintln!("\nInput complete, draining plugin tails...");
                    continue;
                }
                Err(e) => return Err(e).context("Error reading packet"),
            }
        }

        let audio_buf = decode_buffer.as_ref().unwrap();
        let mut offset = 0;
        while offset < current_frame_count {
            let chunk_size = min(current_frame_count - offset, block_size);

            // 1. Prepare input for the first plugin
            for ch in 0..first_plugin_input_count {
                let dst_start = ch * block_size;
                if ch < audio_buf.len() {
                    let src = &audio_buf[ch];
                    input_buffer[dst_start..dst_start + chunk_size].copy_from_slice(&src[offset..offset + chunk_size]);
                    if chunk_size < block_size { input_buffer[dst_start + chunk_size..dst_start + block_size].fill(0.0); }
                } else if !needs_upmix {
                    input_buffer[dst_start..dst_start + block_size].fill(0.0);
                }
            }
            if needs_upmix { input_buffer.copy_within(0..block_size, block_size); }

            // 2. Process through the chain
            for i in 0..instances.len() {
                let (audio_inputs, audio_outputs) = if i == 0 {
                    // First plugin takes from input_buffer
                    let inputs: Vec<&[f32]> = (0..first_plugin_input_count).map(|ch| &input_buffer[ch * block_size..(ch + 1) * block_size]).collect();
                    let outputs: Vec<&mut [f32]> = instances[i].audio_outputs.iter_mut().map(|b| b.as_mut_slice()).collect();
                    (inputs, outputs)
                } else {
                    // Subsequent plugins take from the previous plugin's output
                    let (prev, curr) = split_at_mut(instances, i);
                    let inputs: Vec<&[f32]> = prev.last().unwrap().audio_outputs.iter().map(|b| b.as_slice()).collect();
                    let outputs: Vec<&mut [f32]> = curr[0].audio_outputs.iter_mut().map(|b| b.as_mut_slice()).collect();
                    (inputs, outputs)
                };

                for seq in &mut instances[i].atom_sequence_inputs { seq.clear(); }
                for seq in &mut instances[i].atom_sequence_outputs { seq.clear(); }

                let ports = EmptyPortConnections::new(block_size)
                    .with_control_inputs(instances[i].control_inputs.iter())
                    .with_control_outputs(instances[i].control_outputs.iter_mut())
                    .with_audio_inputs(audio_inputs.into_iter())
                    .with_audio_outputs(audio_outputs.into_iter())
                    .with_atom_sequence_inputs(instances[i].atom_sequence_inputs.iter())
                    .with_atom_sequence_outputs(instances[i].atom_sequence_outputs.iter_mut());

                unsafe { instances[i].instance.run(ports).map_err(|e| anyhow!("Plugin run failed: {:?}", e))?; }
                worker_manager.run_workers();
            }

            // 3. Write final output
            let final_outputs = &instances.last().unwrap().audio_outputs;
            for frame_idx in 0..chunk_size {
                for ch_buf in final_outputs {
                    writer.write_sample(ch_buf[frame_idx]).context("Failed to write sample")?;
                }
            }

            frames_processed += chunk_size as u64;
            offset += chunk_size;

            if frames_processed % (sample_rate as u64) < block_size as u64 {
                let elapsed = start_time.elapsed().as_secs_f64();
                let current_time = frames_processed as f64 / sample_rate as f64;
                if total_frames > 0 && !is_draining {
                    let progress = frames_decoded as f64 / total_frames as f64;
                    eprint!("\r{:6.1}s | {:.1}% | ETA: {:5.1}s", current_time, progress * 100.0, if progress > 0.0 { elapsed / progress - elapsed } else { 0.0 });
                } else if is_draining {
                    eprint!("\r{:6.1}s | DRAIN: {:.1}%", current_time, (drain_frames - drain_remaining) as f64 / drain_frames as f64 * 100.0);
                }
            }
        }
    }
    eprintln!("\nProcessed {} output frames in {:.2}s", frames_processed, start_time.elapsed().as_secs_f64());
    writer.finalize()?;
    Ok(())
}

fn split_at_mut<T>(slice: &mut [T], index: usize) -> (&mut [T], &mut [T]) {
    slice.split_at_mut(index)
}

fn copy_to_f32_buffer(buf: AudioBufferRef<'_>, result: &mut [Vec<f32>]) {
    match buf {
        AudioBufferRef::U8(b) => { for ch in 0..b.spec().channels.count() { for (i, &s) in b.chan(ch).iter().enumerate() { result[ch][i] = (s as f32 - 128.0) / 128.0; } } }
        AudioBufferRef::U16(b) => { for ch in 0..b.spec().channels.count() { for (i, &s) in b.chan(ch).iter().enumerate() { result[ch][i] = (s as f32 - 32768.0) / 32768.0; } } }
        AudioBufferRef::U24(b) => { for ch in 0..b.spec().channels.count() { for (i, &s) in b.chan(ch).iter().enumerate() { result[ch][i] = (s.0 as f32 - 8388608.0) / 8388608.0; } } }
        AudioBufferRef::U32(b) => { for ch in 0..b.spec().channels.count() { for (i, &s) in b.chan(ch).iter().enumerate() { result[ch][i] = (s as f32 - 2147483648.0) / 2147483648.0; } } }
        AudioBufferRef::S8(b) => { for ch in 0..b.spec().channels.count() { for (i, &s) in b.chan(ch).iter().enumerate() { result[ch][i] = s as f32 / 128.0; } } }
        AudioBufferRef::S16(b) => { for ch in 0..b.spec().channels.count() { for (i, &s) in b.chan(ch).iter().enumerate() { result[ch][i] = s as f32 / 32768.0; } } }
        AudioBufferRef::S24(b) => { for ch in 0..b.spec().channels.count() { for (i, &s) in b.chan(ch).iter().enumerate() { result[ch][i] = s.0 as f32 / 8388608.0; } } }
        AudioBufferRef::S32(b) => { for ch in 0..b.spec().channels.count() { for (i, &s) in b.chan(ch).iter().enumerate() { result[ch][i] = s as f32 / 2147483648.0; } } }
        AudioBufferRef::F32(b) => { for ch in 0..b.spec().channels.count() { result[ch].copy_from_slice(b.chan(ch)); } }
        AudioBufferRef::F64(b) => { for ch in 0..b.spec().channels.count() { for (i, &s) in b.chan(ch).iter().enumerate() { result[ch][i] = s as f32; } } }
    }
}
