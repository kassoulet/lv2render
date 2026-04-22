mod audio;
mod cli;
mod config;
mod plugin;
mod processing;

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use hound::WavWriter;
use livi::{FeaturesBuilder, World, WorkerManager, event::LV2AtomSequence};
use std::io::BufWriter;
use std::sync::Arc;

use audio::setup_input_audio;
use cli::Args;
use config::load_chain_from_yaml;
use plugin::{apply_parameter_settings, find_plugin, PluginInstance, PluginLookup};
use processing::{process_audio, ProcessingContext};

const ATOM_SEQUENCE_SIZE: usize = 1024;

fn main() -> Result<()> {
    let args = Args::parse();
    if args.quiet {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if let Ok(dev_null) = std::fs::File::open("/dev/null") {
                unsafe {
                    libc::dup2(dev_null.as_raw_fd(), libc::STDERR_FILENO);
                }
            }
        }
    }
    println!("Initializing LV2 world...");
    let world = World::new();

    let plugins = if let Some(config_path) = &args.file {
        load_chain_from_yaml(config_path)?
    } else {
        if args.plugins.is_empty() {
            bail!("At least one plugin must be specified via CLI or config file (-f)");
        }
        args.plugins
    };

    println!("Probing input file: {:?}", args.input);
    let mut audio_input = setup_input_audio(&args.input)?;

    println!(
        "Audio specs: SampleRate={}, Channels={}, Total frames={}",
        audio_input.sample_rate, audio_input.num_channels, audio_input.total_frames
    );

    let block_size = args.block_size as usize;
    #[allow(clippy::arc_with_non_send_sync)]
    let worker_manager = Arc::new(WorkerManager::default());
    let features = world.build_features(FeaturesBuilder {
        min_block_length: block_size,
        max_block_length: block_size,
        worker_manager: worker_manager.clone(),
    });

    let mut instances = Vec::new();
    let mut current_channels = audio_input.num_channels;

    for (i, p_setting) in plugins.iter().enumerate() {
        let plugin = match find_plugin(&world, &p_setting.plugin_identifier) {
            PluginLookup::Found(p) => p,
            PluginLookup::NotFound => bail!("Plugin '{}' not found", p_setting.plugin_identifier),
            PluginLookup::Ambiguous(matches) => {
                bail!("Plugin '{}' is ambiguous. Candidates:\n{}", p_setting.plugin_identifier, matches.join("\n"))
            }
        };

        println!("Adding plugin to chain: {} ({})", plugin.name(), plugin.uri());

        let port_counts = plugin.port_counts();
        if i == 0 {
            if audio_input.num_channels != port_counts.audio_inputs {
                if port_counts.audio_inputs == 2 && audio_input.num_channels == 1 {
                    println!("Note: Upmixing mono input to stereo for first plugin");
                } else {
                    bail!(
                        "Channel mismatch: input has {} channels, first plugin expects {}",
                        audio_input.num_channels, port_counts.audio_inputs
                    );
                }
            }
        } else if current_channels != port_counts.audio_inputs {
            bail!(
                "Chain mismatch: plugin {} outputs {} channels, but plugin {} expects {}",
                i, current_channels, i + 1, port_counts.audio_inputs
            );
        }

        let instance = unsafe {
            plugin
                .instantiate(features.clone(), audio_input.sample_rate as f64)
                .map_err(|e| anyhow!("Failed to instantiate plugin: {:?}", e))?
        };

        let mut control_inputs: Vec<f32> = plugin
            .ports_with_type(livi::PortType::ControlInput)
            .map(|p| p.default_value)
            .collect();
        apply_parameter_settings(&plugin, &mut control_inputs, &p_setting.params)?;

        let control_outputs = vec![0.0f32; port_counts.control_outputs];
        let atom_sequence_inputs = (0..port_counts.atom_sequence_inputs)
            .map(|_| LV2AtomSequence::new(&features, ATOM_SEQUENCE_SIZE))
            .collect();
        let atom_sequence_outputs = (0..port_counts.atom_sequence_outputs)
            .map(|_| LV2AtomSequence::new(&features, ATOM_SEQUENCE_SIZE))
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

    let wav_spec = hound::WavSpec {
        channels: current_channels as u16,
        sample_rate: audio_input.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let file = std::fs::File::create(&args.output)?;
    let writer = WavWriter::new(BufWriter::new(file), wav_spec)?;

    println!("Starting processing loop with {} plugins...", instances.len());
    
    let ctx = ProcessingContext {
        instances: &mut instances,
        writer,
        block_size,
        input_channels: audio_input.num_channels,
        worker_manager: &worker_manager,
        drain_seconds: args.drain_seconds,
    };

    process_audio(audio_input.format.as_mut(), audio_input.track, ctx)?;

    println!("Processing complete. Output written to: {:?}", args.output);
    Ok(())
}
