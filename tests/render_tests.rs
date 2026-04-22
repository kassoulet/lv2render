use std::process::Command;
use std::path::PathBuf;

fn run_render_test(input_filename: &str) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let input_path = root.join("tests/audio").join(input_filename);
    let output_dir = root.join("tests/audio/output");
    let output_path = output_dir.join(format!("{}.wav", input_filename));

    if output_path.exists() {
        std::fs::remove_file(&output_path).unwrap();
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("run")
       .arg("--")
       .arg("--input").arg(&input_path)
       .arg("--output").arg(&output_path)
       .arg("--quiet")
       .arg("http://calf.sourceforge.net/plugins/Reverb");

    let status = cmd.status().expect("Failed to execute command");

    assert!(status.success(), "lv2render failed to run with input: {}", input_filename);
    assert!(output_path.exists(), "Output file was not created for input: {}", input_filename);

    let reader = hound::WavReader::open(&output_path).expect("Failed to open output WAV");
    assert!(reader.duration() > 0, "Output WAV is empty for input: {}", input_filename);
}

#[test]
fn test_render_all_audio_files() {
    let audio_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/audio");
    let entries = std::fs::read_dir(audio_dir).expect("Failed to read audio directory");

    for entry in entries {
        let entry = entry.unwrap();
        let path = entry.path();
        
        if path.is_file() {
            let filename = path.file_name().unwrap().to_str().unwrap();
            // Skip existing output files or non-audio if needed
            if filename.starts_with("output_") {
                continue;
            }
            
            println!("Testing file: {}", filename);
            run_render_test(filename);
        }
    }
}
