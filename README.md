# lv2render

A high-performance, CLI-based offline audio processor for LV2 plugins written in Rust.

## Overview

`lv2render` is a headless LV2 plugin host designed specifically for non-real-time (offline) audio processing. It reads an audio file, streams it through a specified LV2 plugin, and writes the processed output to a new file as fast as the CPU allows.

## Features

- **Multi-format audio input**: Supports WAV, FLAC, MP3, AAC, OGG, and more via Symphonia
- **LV2 plugin hosting**: Full LV2 plugin support via livi (lilv wrapper)
- **Worker support**: Synchronous LV2 worker task handling for plugins like `master_me`
- **Parameter control**: List and set plugin control port parameters
- **Channel mapping**: Automatic mono-to-stereo upmixing when needed
- **Float processing**: Internal 32-bit float processing to avoid clipping
- **Plugin draining**: Configurable tail capture for reverb/delay effects
- **Latency detection**: Framework for plugin latency compensation
- **Enhanced progress**: Real-time progress with percentage and ETA
- **Optimized performance**: Pre-allocated buffers for maximum speed

## Installation

```bash
cargo build --release
```

The binary will be at `target/release/lv2render` (or in your configured target directory).

## Usage

```bash
lv2render <PLUGIN_IDENTIFIER> <INPUT_FILE> <OUTPUT_FILE> [OPTIONS]
```

### Arguments

- `PLUGIN_IDENTIFIER`: LV2 plugin URI or unique name substring (e.g., `calf`, `mdaEPiano`, `master_me`)
- `INPUT_FILE`: Path to source audio file (WAV, FLAC, MP3, etc.)
- `OUTPUT_FILE`: Path for the processed output WAV file

### Options

- `--block-size <BLOCK_SIZE>`: Number of samples per processing cycle (default: 1024)
- `--list-params`: Print all available control ports for the selected plugin and exit
- `--set <PARAM=VALUE>`: Set plugin parameter (can be used multiple times)
- `--drain-seconds <DRAIN_SECONDS>`: Seconds of silence to drain after input EOF (default: 2.0)
- `-h, --help`: Print help information
- `-V, --version`: Print version information

## Examples

### List plugin parameters

```bash
lv2render "calf" input.wav output.wav --list-params
```

### Process audio with default parameters

```bash
lv2render "Compressor" input.flac output.wav
```

### Process with custom parameter settings

```bash
lv2render "calf" input.mp3 output.wav --set "Threshold=-20" --set "Ratio=4"
```

### Process with custom block size

```bash
lv2render "Reverb" input.wav output.wav --block-size 2048
```

### Process with custom drain time

```bash
lv2render "Reverb" input.wav output.wav --drain-seconds 5.0
```

## Architecture

### Tech Stack

- **Language**: Rust (Edition 2021)
- **Plugin Hosting**: `livi` (Rust wrapper for `lilv`)
- **Audio I/O**: `symphonia` (multi-format decoding) and `hound` (WAV encoding)
- **CLI parsing**: `clap` with derive features
- **Worker support**: `WorkerManager` for async LV2 worker tasks

### Processing Flow

1. **Initialization**: Create LV2 world and discover plugin
2. **Audio Preparation**: Probe input file to determine sample rate and channels
3. **Plugin Instantiation**: Create plugin instance matching audio specs
4. **Latency Detection**: Check for plugin latency port
5. **Processing Loop**:
   - Decode audio chunks from input file
   - Convert to float and map channels (with proper mono-to-stereo upmix)
   - Process through plugin's `run()` method at full block size
   - Handle worker tasks synchronously
   - Write processed output to WAV file
6. **Drain Phase**: Continue processing silence to capture effect tails
7. **Finalize**: Complete WAV file and report statistics

### Performance Optimizations

- **Buffer Pre-allocation**: All buffers allocated once before processing loop
- **Reduced Copying**: Direct sample conversion minimizes memory operations
- **Block Size Compliance**: Always runs plugin at configured block size
- **Progress Reporting**: ETA and percentage calculated from total frames

### Channel Handling

- Mono input → Stereo plugin: Automatic upmix (duplicate channel)
- Sample format conversion: All formats converted to 32-bit float internally
- Output: Always 32-bit float WAV file

## Limitations

- Output format is always 32-bit float WAV
- Latency compensation framework is in place but full implementation requires plugin-specific handling
- MIDI/Atom sequence ports not yet supported
- Only processes first audio track found in file

## License

MIT/Apache-2.0 (same as project template)
