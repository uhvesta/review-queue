use std::{
    env, fs,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use minisign_verify::{PublicKey, Signature};

fn decode_wrapped(path: &Path, kind: &str) -> Result<String, String> {
    let encoded = fs::read_to_string(path)
        .map_err(|error| format!("could not read {kind} {}: {error}", path.display()))?;
    let decoded = STANDARD
        .decode(encoded.trim())
        .map_err(|error| format!("could not decode wrapped {kind}: {error}"))?;
    String::from_utf8(decoded).map_err(|_| format!("{kind} was not valid UTF-8"))
}

fn verify(artifact: &Path, signature_path: &Path, public_key_path: &Path) -> Result<(), String> {
    let public_key = PublicKey::decode(&decode_wrapped(public_key_path, "public key")?)
        .map_err(|error| format!("invalid updater public key: {error}"))?;
    let signature = Signature::decode(&decode_wrapped(signature_path, "signature")?)
        .map_err(|error| format!("invalid updater signature: {error}"))?;
    let artifact_bytes = fs::read(artifact)
        .map_err(|error| format!("could not read artifact {}: {error}", artifact.display()))?;
    public_key
        .verify(&artifact_bytes, &signature, false)
        .map_err(|error| format!("updater signature verification failed: {error}"))
}

fn main() {
    let mut arguments = env::args_os().skip(1).map(PathBuf::from);
    let artifact = arguments.next();
    let signature = arguments.next();
    let public_key = arguments.next();
    if artifact.is_none()
        || signature.is_none()
        || public_key.is_none()
        || arguments.next().is_some()
    {
        eprintln!("usage: verify-updater-signature <artifact> <signature.sig> <public-key.pub>");
        std::process::exit(64);
    }

    if let Err(error) = verify(
        artifact.as_deref().expect("checked"),
        signature.as_deref().expect("checked"),
        public_key.as_deref().expect("checked"),
    ) {
        eprintln!("{error}");
        std::process::exit(1);
    }

    println!("updater signature verification passed");
}
