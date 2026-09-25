//! The API key in the macOS Keychain (spec §2): the app's credential store. The key is read by the
//! backend only; the frontend learns whether one is stored, never the key.
use anyhow::{Context, Result};
use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};

pub const SERVICE: &str = "com.lecturelive.app";
pub const ACCOUNT: &str = "GROK_API_KEY";
/// `errSecItemNotFound`.
const NOT_FOUND: i32 = -25300;

pub fn get(service: &str) -> Result<Option<String>> {
    match get_generic_password(service, ACCOUNT) {
        Ok(bytes) => Ok(Some(String::from_utf8(bytes).context("the stored key is not text")?)),
        Err(e) if e.code() == NOT_FOUND => Ok(None),
        Err(e) => Err(e).context("read the API key from the Keychain"),
    }
}

/// Stores the key, replacing one already there.
pub fn set(service: &str, key: &str) -> Result<()> {
    set_generic_password(service, ACCOUNT, key.as_bytes()).context("store the API key in the Keychain")
}

pub fn delete(service: &str) -> Result<()> {
    match delete_generic_password(service, ACCOUNT) {
        Err(e) if e.code() != NOT_FOUND => Err(e).context("delete the API key from the Keychain"),
        _ => Ok(()),
    }
}

/// The repository's `.env`, where the CLI keeps the key; found at compile time, as the CLI finds it.
const REPO_ENV: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../.env");

/// `GROK_API_KEY` from the environment, else the repository's `.env`: offered once, to move the key
/// the CLI already uses into the Keychain.
pub fn env_key() -> Option<String> {
    std::env::var(ACCOUNT).ok().filter(|v| !v.is_empty()).or_else(|| dotenvy::from_path_iter(REPO_ENV).ok()?.flatten().find(|(k, _)| k == ACCOUNT).map(|(_, v)| v).filter(|v| !v.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the login Keychain under a test service, so it runs by hand:
    /// `cargo test -p desktop keychain -- --ignored`.
    #[test]
    #[ignore]
    fn a_key_is_stored_read_replaced_and_deleted() {
        let service = format!("com.lecturelive.test.{}", uuid::Uuid::new_v4());
        assert_eq!(get(&service).unwrap(), None);
        set(&service, "xai-first").unwrap();
        assert_eq!(get(&service).unwrap().as_deref(), Some("xai-first"));
        set(&service, "xai-second").unwrap();
        assert_eq!(get(&service).unwrap().as_deref(), Some("xai-second"));
        delete(&service).unwrap();
        assert_eq!(get(&service).unwrap(), None);
    }
}
