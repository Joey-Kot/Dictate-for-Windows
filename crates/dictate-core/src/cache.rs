use std::fs;
use std::path::{Path, PathBuf};

use chrono::Local;
use uuid::Uuid;

use crate::Config;

pub const CONVERTED_FILE_SUFFIX: &str = "_convert";

pub fn initialize_cache_dir(config: &mut Config) -> PathBuf {
    if !config.cache_dir.is_empty() {
        let requested = PathBuf::from(&config.cache_dir);
        let absolute = if requested.is_absolute() {
            requested
        } else {
            match std::env::current_dir() {
                Ok(cwd) => cwd.join(requested),
                Err(_) => PathBuf::new(),
            }
        };
        if !absolute.as_os_str().is_empty()
            && ((absolute.is_dir()) || fs::create_dir_all(&absolute).is_ok())
        {
            config.cache_dir = absolute.to_string_lossy().into_owned();
            return absolute;
        }
        config.cache_dir.clear();
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn cleanup_old_temp_items(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("RecordTemp_")
        {
            continue;
        }
        let path = entry.path();
        let _ = if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
    }
}

pub fn temporary_output_path(directory: &Path, extension: &str) -> PathBuf {
    let id = Uuid::new_v4().simple().to_string();
    directory.join(format!("RecordTemp_{}.{}", &id[..16], extension))
}

pub fn recording_output_path(wav_path: &Path, container: &str) -> PathBuf {
    let extension = crate::config::container_extension(container);
    let mut output = wav_path.with_extension(&extension);
    if paths_equal(wav_path, &output) {
        let stem = wav_path
            .file_stem()
            .map(|stem| stem.to_string_lossy())
            .unwrap_or_default();
        output.set_file_name(format!("{stem}{CONVERTED_FILE_SUFFIX}.{extension}"));
    }
    output
}

pub fn handle_cache(
    config: &Config,
    wav_path: Option<&Path>,
    output_path: Option<&Path>,
    upload_succeeded: bool,
    response: &[u8],
) {
    if !(config.keep_cache && !config.cache_dir.is_empty()) {
        if let Some(path) = wav_path {
            let _ = fs::remove_file(path);
        }
        if let Some(path) = output_path {
            let _ = fs::remove_file(path);
        }
        return;
    }

    let directory = Path::new(&config.cache_dir);
    let base = format!("audio-{}", Local::now().format("%Y-%m-%d-%H.%M.%S"));
    let mut cached_wav = None;
    if let Some(path) = wav_path {
        let target = directory.join(format!("{base}{}", extension_with_dot(path)));
        if fs::rename(path, &target).is_ok() {
            cached_wav = Some(target);
        } else {
            let _ = fs::remove_file(path);
        }
    }
    if let Some(path) = output_path {
        let suffix = extension_with_dot(path);
        let mut target = directory.join(format!("{base}{suffix}"));
        if cached_wav
            .as_ref()
            .is_some_and(|wav| paths_equal(wav, &target))
        {
            target = directory.join(format!("{base}{CONVERTED_FILE_SUFFIX}{suffix}"));
        }
        if fs::rename(path, target).is_err() {
            let _ = fs::remove_file(path);
        }
    }
    if upload_succeeded && !response.is_empty() {
        let _ = fs::write(directory.join(format!("{base}.json")), response);
    }
}

fn extension_with_dot(path: &Path) -> String {
    path.extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default()
}

fn paths_equal(first: &Path, second: &Path) -> bool {
    first
        .to_string_lossy()
        .eq_ignore_ascii_case(&second.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes_and_absolutizes_cache() {
        let root = tempfile::tempdir().unwrap();
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(root.path()).unwrap();
        let mut cfg = Config {
            cache_dir: "cache/subdir".into(),
            ..Config::default()
        };
        let result = initialize_cache_dir(&mut cfg);
        std::env::set_current_dir(previous).unwrap();
        assert!(result.is_absolute());
        assert!(result.is_dir());
        assert_eq!(Path::new(&cfg.cache_dir), result);
    }

    #[test]
    fn cleanup_removes_files_and_directories_with_prefix_only() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("RecordTemp_old.wav"), b"x").unwrap();
        fs::create_dir(dir.path().join("RecordTemp_dir")).unwrap();
        fs::write(dir.path().join("keep.wav"), b"x").unwrap();
        cleanup_old_temp_items(dir.path());
        assert!(!dir.path().join("RecordTemp_old.wav").exists());
        assert!(!dir.path().join("RecordTemp_dir").exists());
        assert!(dir.path().join("keep.wav").exists());
    }

    #[test]
    fn wav_conversion_never_reuses_input_path() {
        let input = Path::new("temp/RecordTemp_1234567890123456.wav");
        assert_eq!(
            recording_output_path(input, "WAV"),
            PathBuf::from("temp/RecordTemp_1234567890123456_convert.wav")
        );
    }

    #[test]
    fn disabled_cache_removes_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("in.wav");
        let output = dir.path().join("out.ogg");
        fs::write(&wav, b"wav").unwrap();
        fs::write(&output, b"out").unwrap();
        handle_cache(&Config::default(), Some(&wav), Some(&output), true, b"{}");
        assert!(!wav.exists());
        assert!(!output.exists());
    }
}
