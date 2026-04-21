use anyhow::{Context, Result, anyhow};
use hound::WavWriter;
use livi::{EmptyPortConnections, WorkerManager};
use std::cmp::min;
use std::io::BufWriter;
use std::sync::Arc;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatReader, Track};

use crate::plugin::PluginInstance;
use crate::audio::copy_to_f32_buffer;

pub struct ProcessingContext<'a> {
    pub instances: &'a mut [PluginInstance],
    pub writer: WavWriter<BufWriter<std::fs::File>>,
    pub block_size: usize,
    pub input_channels: usize,
    pub worker_manager: &'a Arc<WorkerManager>,
    pub drain_seconds: f64,
}

struct ProgressReporter {
    start_time: std::time::Instant,
    sample_rate: u32,
    total_frames: u64,
}

impl ProgressReporter {
    fn new(sample_rate: u32, total_frames: u64) -> Self {
        Self {
            start_time: std::time::Instant::now(),
            sample_rate,
            total_frames,
        }
    }

    fn tick(&self, frames_processed: u64, frames_decoded: u64, is_draining: bool, drain_frames: usize, drain_remaining: usize, block_size: usize) {
        if frames_processed % (self.sample_rate as u64) < block_size as u64 {
            let elapsed = self.start_time.elapsed().as_secs_f64();
            let current_time = frames_processed as f64 / self.sample_rate as f64;
            if self.total_frames > 0 && !is_draining {
                let progress = frames_decoded as f64 / self.total_frames as f64;
                let eta = if progress > 0.0 { elapsed / progress - elapsed } else { 0.0 };
                eprint!("\r{:6.1}s | {:.1}% | ETA: {:5.1}s", current_time, progress * 100.0, eta);
            } else if is_draining {
                let drain_progress = (drain_frames - drain_remaining) as f64 / drain_frames as f64 * 100.0;
                eprint!("\r{:6.1}s | DRAIN: {:.1}%", current_time, drain_progress);
            }
        }
    }

    fn finish(&self, frames_processed: u64) {
        eprintln!("\nProcessed {} output frames in {:.2}s", frames_processed, self.start_time.elapsed().as_secs_f64());
    }
}

pub fn process_audio(
    format: &mut dyn FormatReader,
    track: Track,
    mut ctx: ProcessingContext<'_>,
) -> Result<()> {
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let total_frames = track.codec_params.n_frames.unwrap_or(0);
    
    let first_plugin_input_count = ctx.instances[0].plugin.port_counts().audio_inputs;
    let mut input_buffer = vec![0.0f32; ctx.block_size * first_plugin_input_count];
    let needs_upmix = ctx.input_channels == 1 && first_plugin_input_count == 2;

    let drain_frames = (ctx.drain_seconds * sample_rate as f64) as usize;
    let mut drain_remaining = drain_frames;
    let mut is_draining = false;
    let mut frames_processed = 0u64;
    let mut frames_decoded = 0u64;
    
    let reporter = ProgressReporter::new(sample_rate, total_frames);
    let mut decode_buffer: Option<Vec<Vec<f32>>> = None;

    loop {
        let frames_in_chunk;
        if is_draining {
            if drain_remaining == 0 { break; }
            let chunk = min(ctx.block_size, drain_remaining);
            frames_in_chunk = chunk;
            let buf = decode_buffer.get_or_insert_with(|| vec![vec![0.0f32; ctx.block_size]; ctx.input_channels]);
            for ch_buf in buf.iter_mut().take(ctx.input_channels) {
                ch_buf.resize(chunk, 0.0);
                ch_buf.fill(0.0);
            }
            drain_remaining = drain_remaining.saturating_sub(chunk);
        } else {
            match format.next_packet() {
                Ok(packet) => match decoder.decode(&packet) {
                    Ok(decoded) => {
                        let num_frames = decoded.frames();
                        let num_chans = decoded.spec().channels.count();
                        let buf = decode_buffer.get_or_insert_with(|| vec![vec![0.0f32; 0]; num_chans]);
                        for ch_buf in buf.iter_mut().take(num_chans) {
                            ch_buf.resize(num_frames, 0.0);
                        }
                        copy_to_f32_buffer(decoded, buf);
                        frames_in_chunk = num_frames;
                        frames_decoded += num_frames as u64;
                    }
                    Err(e) => {
                        eprintln!("\nWarning: decode error: {}", e);
                        continue;
                    }
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
        while offset < frames_in_chunk {
            let chunk_size = min(frames_in_chunk - offset, ctx.block_size);

            for ch in 0..first_plugin_input_count {
                let dst_start = ch * ctx.block_size;
                if ch < audio_buf.len() {
                    let src = &audio_buf[ch];
                    input_buffer[dst_start..dst_start + chunk_size].copy_from_slice(&src[offset..offset + chunk_size]);
                    if chunk_size < ctx.block_size {
                        input_buffer[dst_start + chunk_size..dst_start + ctx.block_size].fill(0.0);
                    }
                } else if !needs_upmix {
                    input_buffer[dst_start..dst_start + ctx.block_size].fill(0.0);
                }
            }
            if needs_upmix {
                input_buffer.copy_within(0..ctx.block_size, ctx.block_size);
            }

            for i in 0..ctx.instances.len() {
                let (prev, curr) = ctx.instances.split_at_mut(i);
                let inst = &mut curr[0];

                let (audio_inputs, audio_outputs) = if i == 0 {
                    let inputs: Vec<&[f32]> = (0..first_plugin_input_count).map(|ch| &input_buffer[ch * ctx.block_size..(ch + 1) * ctx.block_size]).collect();
                    let outputs: Vec<&mut [f32]> = inst.audio_outputs.iter_mut().map(|b| b.as_mut_slice()).collect();
                    (inputs, outputs)
                } else {
                    let prev_inst = &prev[i-1];
                    let inputs: Vec<&[f32]> = prev_inst.audio_outputs.iter().map(|b| b.as_slice()).collect();
                    let outputs: Vec<&mut [f32]> = inst.audio_outputs.iter_mut().map(|b| b.as_mut_slice()).collect();
                    (inputs, outputs)
                };

                for seq in &mut inst.atom_sequence_inputs { seq.clear(); }
                for seq in &mut inst.atom_sequence_outputs { seq.clear(); }

                let ports = EmptyPortConnections::new(ctx.block_size)
                    .with_control_inputs(inst.control_inputs.iter())
                    .with_control_outputs(inst.control_outputs.iter_mut())
                    .with_audio_inputs(audio_inputs.into_iter())
                    .with_audio_outputs(audio_outputs.into_iter())
                    .with_atom_sequence_inputs(inst.atom_sequence_inputs.iter())
                    .with_atom_sequence_outputs(inst.atom_sequence_outputs.iter_mut());

                unsafe { inst.instance.run(ports).map_err(|e| anyhow!("Plugin run failed: {:?}", e))?; }
                ctx.worker_manager.run_workers();
            }

            let final_outputs = &ctx.instances.last().unwrap().audio_outputs;
            for frame_idx in 0..chunk_size {
                for ch_buf in final_outputs {
                    ctx.writer.write_sample(ch_buf[frame_idx]).context("Failed to write sample")?;
                }
            }

            frames_processed += chunk_size as u64;
            offset += chunk_size;

            reporter.tick(frames_processed, frames_decoded, is_draining, drain_frames, drain_remaining, ctx.block_size);
        }
    }
    
    reporter.finish(frames_processed);
    ctx.writer.finalize()?;
    Ok(())
}
