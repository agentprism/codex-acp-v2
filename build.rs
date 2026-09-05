use std::{env, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=CODEX_ACP_BUNDLE_PATH");
    println!("cargo:rerun-if-env-changed=CODEX_ACP_BUNDLE_SHA256");
    if env::var_os("CARGO_FEATURE_BUNDLED_BACKEND").is_some() {
        let payload = env::var("CODEX_ACP_BUNDLE_PATH")?;
        let digest = env::var("CODEX_ACP_BUNDLE_SHA256")?;
        if !Path::new(&payload).is_absolute() || !Path::new(&payload).is_file() {
            return Err(
                "CODEX_ACP_BUNDLE_PATH must identify an absolute prepared payload file".into(),
            );
        }
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("CODEX_ACP_BUNDLE_SHA256 must be a lowercase SHA-256 digest".into());
        }
        println!("cargo:rerun-if-changed={payload}");
    }
    Ok(())
}
