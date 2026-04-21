use anyhow::{Context, Result};
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::formats::{FormatOptions, FormatReader, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;
use std::path::Path;

pub struct AudioInput {
    pub format: Box<dyn FormatReader>,
    pub track: Track,
    pub sample_rate: u32,
    pub num_channels: usize,
    pub total_frames: u64,
}

pub fn setup_input_audio(input_path: &Path) -> Result<AudioInput> {
    let file = std::fs::File::open(input_path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = input_path.extension() {
        hint.with_extension(&ext.to_string_lossy());
    }
    let probe = symphonia::default::get_probe();
    let result = probe.format(&hint, mss, &FormatOptions::default(), &Default::default())?;
    let format = result.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .context("No audio")?
        .clone();
    let sample_rate = track.codec_params.sample_rate.context("No sample rate")?;
    let num_channels = track.codec_params.channels.map(|ch| ch.count()).context("No channels")?;
    let total_frames = track.codec_params.n_frames.unwrap_or(0);
    
    Ok(AudioInput {
        format,
        track,
        sample_rate,
        num_channels,
        total_frames,
    })
}

use symphonia::core::conv::IntoSample;

pub fn copy_to_f32_buffer(buf: AudioBufferRef<'_>, result: &mut [Vec<f32>]) {
    macro_rules! convert_buf {
        ($buf:expr) => {
            for (ch, dst_ch) in result.iter_mut().enumerate().take($buf.spec().channels.count()) {
                for (i, &s) in $buf.chan(ch).iter().enumerate() {
                    dst_ch[i] = s.into_sample();
                }
            }
        };
    }

    match buf {
        AudioBufferRef::U8(b) => convert_buf!(b),
        AudioBufferRef::U16(b) => convert_buf!(b),
        AudioBufferRef::U24(b) => convert_buf!(b),
        AudioBufferRef::U32(b) => convert_buf!(b),
        AudioBufferRef::S8(b) => convert_buf!(b),
        AudioBufferRef::S16(b) => convert_buf!(b),
        AudioBufferRef::S24(b) => convert_buf!(b),
        AudioBufferRef::S32(b) => convert_buf!(b),
        AudioBufferRef::F32(b) => {
            for (ch, dst_ch) in result.iter_mut().enumerate().take(b.spec().channels.count()) {
                dst_ch.copy_from_slice(b.chan(ch));
            }
        }
        AudioBufferRef::F64(b) => convert_buf!(b),
    }
}
