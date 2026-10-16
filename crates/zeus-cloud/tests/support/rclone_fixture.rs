//! Hermetic rclone command fixture. Only compiled with `test-fixtures`.
#![forbid(unsafe_code)]
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_index = args
        .iter()
        .position(|v| v == "--config")
        .expect("explicit config");
    let config = PathBuf::from(&args[config_index + 1]);
    let root = config.parent().expect("fixture root");
    let mode = std::fs::read_to_string(&config).unwrap_or_default();
    if mode.trim() == "flood" {
        let _ = std::io::stderr().write_all(&vec![b'x'; 128 * 1024]);
        std::process::exit(1);
    }
    if mode.trim() == "fail" {
        eprintln!(
            "ZEUS-CANARY-provider-token source=https://example.invalid/?secret=ZEUS-CANARY-query"
        );
        std::process::exit(2);
    }
    if args.first().map(String::as_str) == Some("backend") {
        println!(
            "{{\"Name\":\"fixture\",\"Features\":{{\"IsLocal\":false,\"PutStream\":false}},\"Hashes\":[\"sha256\"]}}"
        );
        return;
    }
    let remote = args
        .iter()
        .find(|v| v.starts_with("fixture:"))
        .expect("remote");
    let name = remote
        .rsplit('/')
        .next()
        .expect("filename")
        .trim_start_matches("fixture:");
    assert!(!name.contains(['/', '\\', ':']));
    let object = root.join(name);
    match args[0].as_str() {
        "lsjson" => {
            if !object.exists() {
                std::process::exit(4);
            }
            let bytes = std::fs::read(&object).expect("fixture object");
            println!(
                "{}",
                serde_json::json!({"Name":name,"Size":bytes.len(),"IsDir":false,"Hashes":{"SHA-256":hex::encode(Sha256::digest(&bytes))}})
            );
        }
        "rcat" => {
            let size_index = args.iter().position(|v| v == "--size").expect("exact size");
            let size: usize = args[size_index + 1].parse().expect("size");
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes).expect("stdin");
            if bytes.len() != size {
                std::process::exit(2);
            }
            std::fs::write(object, bytes).expect("fixture write");
        }
        "deletefile" => {
            let _ = std::fs::remove_file(object);
        }
        _ => std::process::exit(2),
    }
}
