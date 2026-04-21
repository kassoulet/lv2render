# lv2render

A high-performance, CLI-based offline audio processor for LV2 plugins written in Rust.

## Overview

`lv2render` is a headless LV2 plugin host designed specifically for non-real-time (offline) audio processing. It reads an audio file, streams it through a chain of LV2 plugins, and writes the processed output to a new file as fast as the CPU allows.

## Features

- **Sequential Plugin Chaining**: Apply multiple plugins in a single pass.
- **Flexible Configuration**: Define effect chains via CLI arguments or YAML files.
- **Multi-format audio input**: Supports WAV, FLAC, MP3, AAC, OGG, and more via Symphonia.
- **LV2 plugin hosting**: Full LV2 plugin support via livi (lilv wrapper).
- **Worker support**: Synchronous LV2 worker task handling for plugins like `master_me`.
- **Parameter control**: Override any control port parameter.
- **Channel mapping**: Automatic mono-to-stereo upmixing for the first plugin in the chain.
- **Float processing**: Internal 32-bit float processing to maintain high fidelity.
- **Plugin draining**: Configurable tail capture for reverb/delay effects.
- **Optimized performance**: Pre-allocated buffers and zero-allocation hot loop.

## Installation

```bash
cargo build --release
```

The binary will be at `target/release/lv2render`.

## Usage

```bash
lv2render -i <INPUT_FILE> -o <OUTPUT_FILE> [CHAIN...] [OPTIONS]
```

### Arguments

- `-i, --input <INPUT_FILE>`: Path to source audio file.
- `-o, --output <OUTPUT_FILE>`: Path for the processed output WAV file.
- `[CHAIN...]`: A list of plugins to apply. Format: `"plugin_name:param1=val1:param2=val2"`.

### Options

- `-f, --file <YAML_FILE>`: Path to a YAML file defining the effect chain.
- `--block-size <BLOCK_SIZE>`: Samples per processing cycle (default: 1024).
- `--drain-seconds <SECONDS>`: Seconds of silence to process after EOF (default: 2.0).

## Chain Configuration

### CLI Chain

Plugins are applied in the order they appear on the command line.

```bash
lv2render -i in.wav -o out.wav "Calf Reverb:Decay time=0.5" "master_me:input gain=3" "Calf Bass Enhancer"
```

### YAML Chain (`-f`)

For complex setups, use a YAML file. It supports multiple formats:

```yaml
# effect-chain.yaml
- "Calf Reverb:Decay time=0.5" # String format
- master_me:                   # Key-List format
    - leveler bypass=1
    - input gain=3
- Calf Bass Enhancer           # Simple name
```

Run with:
```bash
lv2render -i input.wav -o output.wav -f effect-chain.yaml
```

## Architecture

### Processing Flow

1. **Initialization**: Discover all plugins in the chain.
2. **Audio Preparation**: Probe input file for sample rate and channels.
3. **Chain Setup**: Instantiate plugins and verify channel compatibility between stages.
4. **Processing Loop**:
   - Decode audio chunks.
   - Flow audio through the plugin chain (Buffer swapping between stages).
   - Handle worker tasks synchronously for each plugin.
   - Write final output to WAV.
5. **Drain Phase**: Capture tails (reverb/delay) for all plugins in the chain.

### Channel Handling

- **First Plugin**: Supports mono-to-stereo upmix if the input is mono and the plugin is stereo.
- **The Chain**: Each plugin must have an input channel count matching the output of the previous plugin.
- **Output**: Always 32-bit float WAV.

## License

MIT/Apache-2.0
