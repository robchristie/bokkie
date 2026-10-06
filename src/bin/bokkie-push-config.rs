//! Explicit offline VAPID setup; never contacts a notification service.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::Parser;
use openssl::{
    ec::{EcGroup, EcKey},
    nid::Nid,
};
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Options {
    /// New absolute private JSON file. Existing files are never overwritten.
    #[arg(long)]
    output: PathBuf,
    /// Bokkie's HTTPS contact origin, such as https://bokkie.yutani.tech.
    #[arg(long)]
    subject: String,
}
fn create(path: &Path, subject: &str) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent().ok_or("An absolute output file is required")?;
    if !path.is_absolute() || parent.canonicalize()? != parent || !parent.is_dir() {
        return Err("Use an absolute file beneath an existing canonical directory".into());
    }
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?;
    let key = EcKey::generate(&group)?;
    let private = URL_SAFE_NO_PAD.encode(key.private_key().to_vec_padded(32)?);
    let config = bokkie::notifications::push::PushConfig {
        vapid_private_key: private.clone(),
        subject: subject.into(),
        timeout_ms: 2000,
        ttl_seconds: 3600,
    };
    config.validate()?;
    let json = serde_json::to_vec_pretty(
        &serde_json::json!({"vapid_private_key":private,"subject":subject,"timeout_ms":2000,"ttl_seconds":3600}),
    )?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&json)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse();
    create(&options.output, &options.subject)?;
    println!(
        "Created private push configuration at {}. Preserve its key across restarts; device enrolment remains a separate explicit action.",
        options.output.display()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn offline_configuration_is_private_loadable_and_never_overwrites_an_existing_key() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("push.json");
        create(&path, "https://bokkie.example.org").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(bokkie::notifications::push::PushConfig::load(&path).is_ok());
        assert!(create(&path, "https://bokkie.example.org").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(
            create(
                &temp.path().join("invalid.json"),
                "http://untrusted.example.org"
            )
            .is_err()
        );
        assert!(!temp.path().join("invalid.json").exists());
    }
}
