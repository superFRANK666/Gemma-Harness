use base64::{engine::general_purpose::STANDARD, Engine};
use minisign_verify::{PublicKey, Signature};
use serde_json::Value;
use std::{error::Error, fs, io::Read, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::args()
        .nth(1)
        .ok_or("expected release asset directory")?;
    let directory = Path::new(&directory);
    let config: Value =
        serde_json::from_slice(&fs::read("apps/desktop/src-tauri/tauri.conf.json")?)?;
    let encoded_key = config["plugins"]["updater"]["pubkey"]
        .as_str()
        .ok_or("missing updater public key")?;
    let key_text = String::from_utf8(STANDARD.decode(encoded_key)?)?;
    let key = PublicKey::decode(&key_text)?;
    let manifest: Value = serde_json::from_slice(&fs::read(directory.join("latest.json"))?)?;
    let version = manifest["version"]
        .as_str()
        .ok_or("missing update version")?;
    let platforms = manifest["platforms"]
        .as_object()
        .ok_or("missing update platforms")?;
    if platforms.len() != 6 {
        return Err("expected six updater platform variants".into());
    }
    let expected_root =
        format!("https://github.com/superFRANK666/Gemma-Harness/releases/download/v{version}/");
    for (platform, entry) in platforms {
        let url = entry["url"].as_str().ok_or("missing update URL")?;
        let filename = url
            .strip_prefix(&expected_root)
            .ok_or("unexpected update URL")?;
        if filename.is_empty() || filename.contains(['/', '\\', '%']) {
            return Err(format!("invalid update filename for {platform}").into());
        }
        let encoded_signature = entry["signature"].as_str().ok_or("missing signature")?;
        let disk_signature = fs::read_to_string(directory.join(format!("{filename}.sig")))?;
        if disk_signature.trim() != encoded_signature {
            return Err(format!("manifest signature does not match {filename}.sig").into());
        }
        let signature_text = String::from_utf8(STANDARD.decode(encoded_signature)?)?;
        let signature = Signature::decode(&signature_text)?;
        let signed_version = signature
            .trusted_comment()
            .split_whitespace()
            .find_map(|part| part.strip_prefix("version:"));
        if signed_version != Some(version) {
            return Err(format!("signed version mismatch for {platform}").into());
        }
        let mut verifier = key.verify_stream(&signature)?;
        let mut file = fs::File::open(directory.join(filename))?;
        let mut buffer = [0u8; 65536];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            verifier.update(&buffer[..read]);
        }
        verifier.finalize()?;
        println!("Verified signed updater artifact for {platform}");
    }
    Ok(())
}
