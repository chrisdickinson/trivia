use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

const BASE_URL: &str = "https://huggingface.co/Qdrant/all-MiniLM-L6-v2-onnx/resolve/main";

struct File {
    name: &'static str,
    sha256: &'static str,
}

const FILES: &[File] = &[
    File {
        name: "model.onnx",
        sha256: "bbd7b466f6d58e646fdc2bd5fd67b2f5e93c0b687011bd4548c420f7bd46f0c5",
    },
    File {
        name: "tokenizer.json",
        sha256: "da0e79933b9ed51798a3ae27893d3c5fa4a201126cef75586296df9b4d2c62a0",
    },
    File {
        name: "config.json",
        sha256: "1b4d8e2a3988377ed8b519a31d8d31025a25f1c5f8606998e8014111438efcd7",
    },
    File {
        name: "special_tokens_map.json",
        sha256: "5d5b662e421ea9fac075174bb0688ee0d9431699900b90662acd44b2a350503a",
    },
    File {
        name: "tokenizer_config.json",
        sha256: "bd2e06a5b20fd1b13ca988bedc8763d332d242381b4fbc98f8fead4524158f79",
    },
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=TRIVIA_MODEL_DIR");
    println!("cargo:rerun-if-env-changed=TRIVIA_MODEL_BASE_URL");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    let src_dir = match std::env::var("TRIVIA_MODEL_DIR") {
        Ok(dir) => Some(PathBuf::from(dir)),
        Err(_) => None,
    };
    let base_url = std::env::var("TRIVIA_MODEL_BASE_URL").unwrap_or_else(|_| BASE_URL.to_string());

    for file in FILES {
        let dest = out_dir.join(file.name);

        if dest.exists() && sha256_file(&dest) == file.sha256 {
            continue;
        }

        if let Some(src) = &src_dir {
            let from = src.join(file.name);
            if !from.exists() {
                panic!(
                    "TRIVIA_MODEL_DIR is set but {} is missing from {}",
                    file.name,
                    src.display()
                );
            }
            std::fs::copy(&from, &dest).unwrap_or_else(|e| {
                panic!("failed to copy {} into OUT_DIR: {e}", file.name);
            });
        } else {
            let url = format!("{}/{}", base_url.trim_end_matches('/'), file.name);
            download(&url, &dest);
        }

        let got = sha256_file(&dest);
        if got != file.sha256 {
            let _ = std::fs::remove_file(&dest);
            panic!(
                "checksum mismatch for {}: expected {}, got {}",
                file.name, file.sha256, got
            );
        }
    }
}

fn download(url: &str, dest: &Path) {
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke curl for {url}: {e}"));

    if !status.success() {
        let _ = std::fs::remove_file(dest);
        panic!(
            "failed to download {url} (curl exit status {status}). \
             Set TRIVIA_MODEL_DIR=/path/to/local/weights to build offline."
        );
    }
}

fn sha256_file(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("failed to read for checksum");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    format!("{:x}", hasher.finalize())
}
