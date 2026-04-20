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

/// lv2render - High-performance offline LV2 audio processor
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// LV2 plugin identifier (URI or unique name substring)
    plugin_identifier: String,

    /// Input audio file path (WAV, FLAC, MP3, etc.)
    input_file: Option<PathBuf>,

    /// Output processed WAV file path
    output_file: Option<PathBuf>,

    /// Number of samples per processing cycle
    #[arg(long, default_value_t = 1024)]
    block_size: u32,

    /// List all available control ports for the plugin and exit
    #[arg(long)]
    list_params: bool,

    /// Set plugin parameter (format: PARAM_NAME=VALUE or PORT_INDEX=VALUE)
    #[arg(long, value_parser = parse_param_setting)]
    set: Vec<(String, f32)>,

    /// Seconds of silence to drain after input EOF (for reverb/delay tails)
    #[arg(long, default_value_t = 2.0)]
    drain_seconds: f64,
}

fn parse_param_setting(s: &str) -> Result<(String, f32), String> {
    let parts: Vec<&str> = s.splitn(2, '=').collect();
    if parts.len() != 2 {
        return Err(format!("Invalid parameter setting: {}. Expected PARAM=VALUE", s));
    }
    let value = parts[1]
        .parse::<f32>()
        .map_err(|_| format!("Invalid numeric value: {}", parts[1]))?;
    Ok((parts[0].to_string(), value))
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Phase A: Initialization
    println!("Initializing LV2 world...");
    let world = World::new();

    // Plugin Discovery
    let plugin = find_plugin(&world, &args.plugin_identifier)
        .with_context(|| format!("Plugin '{}' not found", args.plugin_identifier))?;

    println!("Found plugin: {}", plugin.name());

    if args.list_params {
        list_plugin_ports(&plugin)?;
        return Ok(());
    }

    // Ensure input and output files are provided for processing
    let input_file = args.input_file.ok_or_else(|| anyhow!("Input file is required for processing"))?;
    let output_file = args.output_file.ok_or_else(|| anyhow!("Output file is required for processing"))?;

    // Phase B: Audio Preparation
    println!("Probing input file: {:?}", input_file);
    let (mut format, track, sample_rate, num_channels, total_frames) =
        setup_input_audio(&input_file)?;

    println!(
        "Audio specs: SampleRate={}, Channels={}, Total frames={}",
        sample_rate, num_channels, total_frames
    );

    let port_counts = plugin.port_counts();
    let num_plugin_inputs = port_counts.audio_inputs;
    let num_plugin_outputs = port_counts.audio_outputs;

    println!(
        "Plugin I/O: {} audio inputs, {} audio outputs",
        num_plugin_inputs, num_plugin_outputs
    );

    if num_plugin_inputs == 0 || num_plugin_outputs == 0 {
        bail!("Plugin must have both audio input and output ports");
    }

    // Check channel compatibility
    if num_channels != num_plugin_inputs {
        if num_plugin_inputs == 2 && num_channels == 1 {
            println!("Note: Upmixing mono input to stereo plugin");
        } else {
            bail!(
                "Channel mismatch: input has {} channels, plugin expects {}",
                num_channels,
                num_plugin_inputs
            );
        }
    }

    // Set up features with worker support
    let block_size = args.block_size as usize;
    let worker_manager = Arc::new(WorkerManager::default());
    let features = world.build_features(FeaturesBuilder {
        min_block_length: block_size,
        max_block_length: block_size,
        worker_manager: worker_manager.clone(),
    });

    // Instantiate the plugin
    let sample_rate_f64 = sample_rate as f64;
    let mut instance = unsafe {
        plugin
            .instantiate(features.clone(), sample_rate_f64)
            .map_err(|e| anyhow!("Failed to instantiate plugin: {:?}", e))?
    };

    println!(
        "Plugin instantiated: sample_rate={}, block_size={}",
        sample_rate_f64, block_size
    );

    // Get control input ports and apply user settings
    let mut control_inputs: Vec<f32> = plugin
        .ports_with_type(PortType::ControlInput)
        .map(|p| p.default_value)
        .collect();

    apply_parameter_settings(&plugin, &mut control_inputs, &args.set)?;

    // Check for latency port
    let latency_frames = detect_latency(&plugin, &mut instance, block_size, &control_inputs)?;
    if latency_frames > 0 {
        println!("Plugin reports latency: {} frames ({} ms)", 
            latency_frames,
            (latency_frames as f64 / sample_rate_f64) * 1000.0
        );
    }

    // Initialize output WAV writer (32-bit float)
    let wav_spec = hound::WavSpec {
        channels: num_plugin_outputs as u16,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let file = std::fs::File::create(&output_file)
        .with_context(|| format!("Failed to create output file: {:?}", output_file))?;
    let writer = WavWriter::new(BufWriter::new(file), wav_spec)
        .with_context(|| format!("Failed to create WAV writer for: {:?}", output_file))?;

    // Phase C: Processing Loop
    println!("Starting processing loop...");
    process_audio(
        format.as_mut(),
        track,
        &mut instance,
        writer,
        block_size,
        num_channels,
        num_plugin_inputs,
        num_plugin_outputs,
        &control_inputs,
        &worker_manager,
        &features,
        args.drain_seconds,
    )?;

    println!("Processing complete. Output written to: {:?}", output_file);

    Ok(())
}

/// Find a plugin by URI or name substring
fn find_plugin(world: &World, identifier: &str) -> Option<Plugin> {
    // First try exact URI match
    if let Some(plugin) = world.plugin_by_uri(identifier) {
        return Some(plugin);
    }

    // Try to find by name substring
    let matches: Vec<_> = world
        .iter_plugins()
        .filter(|p| {
            let name = p.name().to_lowercase();
            let id = identifier.to_lowercase();
            name.contains(&id)
        })
        .collect();

    if matches.len() == 1 {
        Some(matches[0].clone())
    } else if matches.len() > 1 {
        println!("Multiple plugins match '{}'. Available options:", identifier);
        for p in &matches {
            println!("  - {} ({})", p.name(), p.uri());
        }
        None
    } else {
        None
    }
}

/// List all control ports for a plugin
fn list_plugin_ports(plugin: &Plugin) -> Result<()> {
    println!("Control ports for plugin: {}", plugin.name());
    println!("{:<5} {:<35} {:<10}", "Idx", "Name", "Default");
    println!("{}", "-".repeat(55));

    for port in plugin.ports_with_type(PortType::ControlInput) {
        println!(
            "{:<5} {:<35} {:<10.4}",
            port.index.0, port.name, port.default_value
        );
    }

    println!("\nAudio ports:");
    println!("{:<5} {:<35} {:<10}", "Idx", "Name", "Direction");
    println!("{}", "-".repeat(55));

    for port in plugin.ports_with_type(PortType::AudioInput) {
        println!("{:<5} {:<35} {:<10}", port.index.0, port.name, "INPUT");
    }

    for port in plugin.ports_with_type(PortType::AudioOutput) {
        println!("{:<5} {:<35} {:<10}", port.index.0, port.name, "OUTPUT");
    }

    Ok(())
}

/// Apply user-specified parameter settings
fn apply_parameter_settings(
    plugin: &Plugin,
    control_inputs: &mut [f32],
    settings: &[(String, f32)],
) -> Result<()> {
    let ports: Vec<_> = plugin.ports_with_type(PortType::ControlInput).collect();

    for (param, value) in settings {
        // Try to find port by name or index
        let port_idx = if let Ok(idx) = param.parse::<usize>() {
            if idx < control_inputs.len() {
                Some(idx)
            } else {
                None
            }
        } else {
            ports.iter().position(|p| {
                p.name.to_lowercase() == param.to_lowercase()
            })
        };

        if let Some(idx) = port_idx {
            control_inputs[idx] = *value;
            println!("  Set port {} to {}", param, value);
        } else {
            bail!("Parameter '{}' not found", param);
        }
    }
    Ok(())
}

/// Detect plugin latency by looking for a "latency" output control port
fn detect_latency(
    plugin: &Plugin,
    instance: &mut Instance,
    block_size: usize,
    control_inputs: &[f32],
) -> Result<u32> {
    // Find latency output port
    let latency_port = plugin
        .ports_with_type(PortType::ControlOutput)
        .find(|p| p.name.to_lowercase().contains("latency"));

    if let Some(_port) = latency_port {
        // Run plugin once with silence to get latency value
        let mut control_outputs = vec![0.0f32; plugin.port_counts().control_outputs];
        let mut audio_outputs = vec![vec![0.0f32; block_size]; plugin.port_counts().audio_outputs];
        
        let audio_inputs_vec: Vec<&[f32]> = vec![];
        let audio_outputs_vec: Vec<&mut [f32]> = audio_outputs
            .iter_mut()
            .map(|buf| buf.as_mut_slice())
            .collect();

        let ports = EmptyPortConnections::new(block_size)
            .with_control_inputs(control_inputs.iter())
            .with_control_outputs(control_outputs.iter_mut())
            .with_audio_inputs(audio_inputs_vec.into_iter())
            .with_audio_outputs(audio_outputs_vec.into_iter());

        unsafe {
            instance.run(ports)
                .map_err(|e| anyhow!("Failed to run plugin for latency detection: {:?}", e))?;
        }

        // The latency value should be in the control_outputs at the port's index
        // For now, we'll return 0 as proper latency compensation requires more complex handling
        // This is a placeholder for future implementation
        return Ok(0);
    }

    Ok(0)
}

/// Set up input audio decoding
fn setup_input_audio(
    input_path: &PathBuf,
) -> Result<(
    Box<dyn symphonia::core::formats::FormatReader>,
    symphonia::core::formats::Track,
    u32,
    usize,
    u64,
)> {
    // Open the media source
    let file = std::fs::File::open(input_path)
        .with_context(|| format!("Cannot open input file: {:?}", input_path))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // Probe the file format
    let mut hint = Hint::new();
    if let Some(ext) = input_path.extension() {
        hint.with_extension(&ext.to_string_lossy());
    }

    let probe = symphonia::default::get_probe();
    let result = probe
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &symphonia::core::meta::MetadataOptions::default(),
        )
        .context("Unsupported audio format or corrupted file")?;

    let format = result.format;

    // Find the first audio track
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("No audio tracks found"))?
        .clone();

    let codec_params = &track.codec_params;
    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("Cannot determine sample rate"))?;

    let num_channels = codec_params
        .channels
        .map(|ch| ch.count())
        .ok_or_else(|| anyhow!("Cannot determine channel count"))?;

    let total_frames = codec_params.n_frames.unwrap_or(0);

    Ok((format, track, sample_rate, num_channels, total_frames))
}

/// Main audio processing loop
fn process_audio(
    format: &mut dyn symphonia::core::formats::FormatReader,
    track: symphonia::core::formats::Track,
    instance: &mut Instance,
    mut writer: WavWriter<BufWriter<std::fs::File>>,
    block_size: usize,
    input_channels: usize,
    num_plugin_inputs: usize,
    num_plugin_outputs: usize,
    control_inputs: &[f32],
    worker_manager: &Arc<WorkerManager>,
    features: &Arc<livi::Features>,
    drain_seconds: f64,
) -> Result<()> {
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let total_frames = track.codec_params.n_frames.unwrap_or(0);

    // Create decoder
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("Unsupported audio codec")?;

    // Pre-allocate all buffers once (fixes hot-loop allocation issue)
    // FIX: Allocation must be based on num_plugin_inputs, not input_channels, to avoid panic on upmix
    let mut input_buffer = vec![0.0f32; block_size * num_plugin_inputs];
    let mut output_buffers: Vec<Vec<f32>> = (0..num_plugin_outputs)
        .map(|_| vec![0.0f32; block_size])
        .collect();
    
    // Pre-allocate control outputs buffer
    let mut control_outputs: Vec<f32> = vec![0.0f32; instance.port_counts().control_outputs];
    
    // Pre-allocate atom sequence buffers for MIDI input (empty sequences)
    let atom_sequence_inputs_count = instance.port_counts().atom_sequence_inputs;
    let atom_sequence_outputs_count = instance.port_counts().atom_sequence_outputs;
    let mut atom_sequence_inputs: Vec<LV2AtomSequence> = (0..atom_sequence_inputs_count)
        .map(|_| LV2AtomSequence::new(&features, 1024))
        .collect();
    let mut atom_sequence_outputs: Vec<LV2AtomSequence> = (0..atom_sequence_outputs_count)
        .map(|_| LV2AtomSequence::new(&features, 1024))
        .collect();
    
    // For mono-to-stereo upmix, we need an extra buffer
    let needs_upmix = input_channels == 1 && num_plugin_inputs == 2;

    // Drain phase: calculate how many frames to drain
    let drain_frames = (drain_seconds * sample_rate as f64) as usize;
    let mut drain_remaining = drain_frames;
    let mut is_draining = false;

    let mut frames_processed = 0u64;
    let mut frames_decoded = 0u64;
    let mut _packets_processed = 0u64;
    let start_time = std::time::Instant::now();

    // Reusable f32 buffer for decoding to avoid per-packet allocations
    let mut decode_buffer: Option<Vec<Vec<f32>>> = None;

    loop {
        // Decode next packet or handle drain phase
        let mut current_frame_count = 0;

        if is_draining {
            if drain_remaining == 0 {
                break;
            }
            let chunk = min(block_size, drain_remaining);
            current_frame_count = chunk;
            
            // Fill decode_buffer with silence for draining
            let buf = decode_buffer.get_or_insert_with(|| vec![vec![0.0f32; block_size]; input_channels]);
            for ch in 0..input_channels {
                buf[ch].resize(chunk, 0.0);
                buf[ch].fill(0.0);
            }
            drain_remaining = drain_remaining.saturating_sub(chunk);
        } else {
            match format.next_packet() {
                Ok(packet) => match decoder.decode(&packet) {
                    Ok(decoded) => {
                        // Use a more efficient conversion that reuses buffers
                        let num_frames = decoded.frames();
                        let num_chans = decoded.spec().channels.count();
                        
                        let buf = decode_buffer.get_or_insert_with(|| vec![vec![0.0f32; 0]; num_chans]);
                        for ch in 0..num_chans {
                            buf[ch].resize(num_frames, 0.0);
                        }
                        
                        copy_to_f32_buffer(decoded, buf);
                        
                        current_frame_count = num_frames;
                        frames_decoded += num_frames as u64;
                    }
                    Err(symphonia::core::errors::Error::DecodeError(_)) => {
                        eprintln!("\nWarning: Decode error, skipping packet");
                        continue;
                    }
                    Err(e) => {
                        return Err(e).context("Error during audio decoding");
                    }
                },
                Err(SymphoniaError::IoError(ref io_err)) 
                    if io_err.kind() == std::io::ErrorKind::UnexpectedEof => 
                {
                    // EOF reached, switch to drain phase
                    is_draining = true;
                    eprintln!("\nInput complete, draining {} seconds of plugin tail...", drain_seconds);
                    continue;
                }
                Err(e) => {
                    return Err(e).context("Error reading next packet");
                }
            }
        }

        let audio_buf = decode_buffer.as_ref().unwrap();
        let frame_count = current_frame_count;
        if frame_count == 0 {
            continue;
        }

        // Process in chunks of exactly block_size (fixes block size contract violation)
        let mut offset = 0;
        while offset < frame_count {
            let remaining = frame_count - offset;
            let chunk_size = min(remaining, block_size);

            // Fill input buffer with audio data (fixes mono upmix and reduces copies)
            for ch in 0..num_plugin_inputs {
                let dst_start = ch * block_size;
                
                if ch < audio_buf.len() {
                    let src = &audio_buf[ch];
                    let copy_len = chunk_size;
                    input_buffer[dst_start..dst_start + copy_len]
                        .copy_from_slice(&src[offset..offset + copy_len]);
                    
                    // Pad with silence if chunk is smaller than block_size
                    if chunk_size < block_size {
                        input_buffer[dst_start + chunk_size..dst_start + block_size].fill(0.0);
                    }
                } else if !needs_upmix {
                    // Fill unused plugin inputs with silence
                    input_buffer[dst_start..dst_start + block_size].fill(0.0);
                }
            }
            
            // Fix mono-to-stereo upmix: duplicate first channel to second (after filling first channel)
            if needs_upmix {
                let dst_start = block_size;
                // Use copy_within to avoid temporary allocation
                input_buffer.copy_within(0..block_size, dst_start);
            }

            // Set up port connections using pre-allocated buffers
            let audio_inputs: Vec<&[f32]> = (0..num_plugin_inputs)
                .map(|ch| {
                    let start = ch * block_size;
                    &input_buffer[start..start + block_size]
                })
                .collect();

            let audio_outputs: Vec<&mut [f32]> = output_buffers
                .iter_mut()
                .map(|buf| &mut buf[..block_size])
                .collect();

            // Clear atom sequences for each chunk
            for seq in atom_sequence_inputs.iter_mut() {
                seq.clear();
            }
            for seq in atom_sequence_outputs.iter_mut() {
                seq.clear();
            }

            // Run the plugin at the full block_size (fixes contract violation)
            let ports = EmptyPortConnections::new(block_size)
                .with_control_inputs(control_inputs.iter())
                .with_control_outputs(control_outputs.iter_mut())
                .with_audio_inputs(audio_inputs.into_iter())
                .with_audio_outputs(audio_outputs.into_iter())
                .with_atom_sequence_inputs(atom_sequence_inputs.iter())
                .with_atom_sequence_outputs(atom_sequence_outputs.iter_mut());

            unsafe {
                instance
                    .run(ports)
                    .map_err(|e| anyhow!("Plugin run failed: {:?}", e))?;
            }

            // Process worker tasks
            worker_manager.run_workers();

            // Write output to WAV file (only write actual samples, not padding)
            let write_frames = chunk_size;
            for frame_idx in 0..write_frames {
                for ch in 0..num_plugin_outputs {
                    writer
                        .write_sample(output_buffers[ch][frame_idx])
                        .context("Failed to write audio sample")?;
                }
            }

            frames_processed += write_frames as u64;
            offset += chunk_size;

            // Enhanced progress indicator with percentage and ETA
            if frames_processed % (sample_rate as u64) < block_size as u64 {
                let elapsed = start_time.elapsed().as_secs_f64();
                let current_time = frames_processed as f64 / sample_rate as f64;
                
                if total_frames > 0 && !is_draining {
                    let progress = frames_decoded as f64 / total_frames as f64;
                    let eta = if progress > 0.0 {
                        elapsed / progress - elapsed
                    } else {
                        0.0
                    };
                    eprint!(
                        "\r{:6.1}s | {:.1}% | ETA: {:5.1}s",
                        current_time,
                        progress * 100.0,
                        eta
                    );
                } else if is_draining {
                    let drain_progress = (drain_frames - drain_remaining) as f64 / drain_frames as f64;
                    eprint!(
                        "\r{:6.1}s | DRAIN: {:.1}%",
                        current_time,
                        drain_progress * 100.0
                    );
                } else {
                    eprint!("\r{:6.1}s", current_time);
                }
            }
        }

        if !is_draining {
            _packets_processed += 1;
        }
    }

    eprintln!(); // Newline after progress
    eprintln!(
        "Processed: {} input frames -> {} output frames in {:.2}s",
        frames_decoded,
        frames_processed,
        start_time.elapsed().as_secs_f64()
    );

    writer.finalize().context("Failed to finalize WAV file")?;
    Ok(())
}

/// Copy any audio buffer type to a pre-allocated f32 buffer
fn copy_to_f32_buffer(buf: AudioBufferRef<'_>, result: &mut [Vec<f32>]) {
    match buf {
        AudioBufferRef::U8(b) => {
            for ch in 0..b.spec().channels.count() {
                for (i, &sample) in b.chan(ch).iter().enumerate() {
                    result[ch][i] = (sample as f32 - 128.0) / 128.0;
                }
            }
        }
        AudioBufferRef::U16(b) => {
            for ch in 0..b.spec().channels.count() {
                for (i, &sample) in b.chan(ch).iter().enumerate() {
                    result[ch][i] = (sample as f32 - 32768.0) / 32768.0;
                }
            }
        }
        AudioBufferRef::U24(b) => {
            for ch in 0..b.spec().channels.count() {
                for (i, &sample) in b.chan(ch).iter().enumerate() {
                    // U24: 0 to 16777215, bias at 8388608
                    result[ch][i] = (sample.0 as f32 - 8388608.0) / 8388608.0;
                }
            }
        }
        AudioBufferRef::U32(b) => {
            for ch in 0..b.spec().channels.count() {
                for (i, &sample) in b.chan(ch).iter().enumerate() {
                    // U32: 0 to 4294967295, bias at 2147483648
                    result[ch][i] = (sample as f32 - 2147483648.0) / 2147483648.0;
                }
            }
        }
        AudioBufferRef::S8(b) => {
            for ch in 0..b.spec().channels.count() {
                for (i, &sample) in b.chan(ch).iter().enumerate() {
                    result[ch][i] = sample as f32 / 128.0;
                }
            }
        }
        AudioBufferRef::S16(b) => {
            for ch in 0..b.spec().channels.count() {
                for (i, &sample) in b.chan(ch).iter().enumerate() {
                    result[ch][i] = sample as f32 / 32768.0;
                }
            }
        }
        AudioBufferRef::S24(b) => {
            for ch in 0..b.spec().channels.count() {
                for (i, &sample) in b.chan(ch).iter().enumerate() {
                    // S24: -8388608 to 8388607
                    result[ch][i] = sample.0 as f32 / 8388608.0;
                }
            }
        }
        AudioBufferRef::S32(b) => {
            for ch in 0..b.spec().channels.count() {
                for (i, &sample) in b.chan(ch).iter().enumerate() {
                    // S32: -2147483648 to 2147483647
                    result[ch][i] = sample as f32 / 2147483648.0;
                }
            }
        }
        AudioBufferRef::F32(b) => {
            for ch in 0..b.spec().channels.count() {
                result[ch].copy_from_slice(b.chan(ch));
            }
        }
        AudioBufferRef::F64(b) => {
            for ch in 0..b.spec().channels.count() {
                for (i, &sample) in b.chan(ch).iter().enumerate() {
                    result[ch][i] = sample as f32;
                }
            }
        }
    }
}
