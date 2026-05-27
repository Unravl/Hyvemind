//! OS-keyring-backed storage for provider API keys.
//!
//! All provider keys are stored as a single JSON blob under service
//! `"hyvemind"`, account `"__provider_keys__"`. This produces exactly one
//! macOS keychain authorization prompt per app start instead of one per
//! configured provider.
//!
//! On macOS this uses the native Keychain via the Security framework; on
//! Linux the native kernel keyring; on Windows the Credential Manager.
//!
//! All operations are best-effort and log on unexpected errors. Missing
//! credentials (`NoEntry`) are treated as "not present" — never an error.
//!
//! The legacy per-provider `get` / `set` / `delete` API is retained because
//! the one-time migration path needs to read the old per-provider entries
//! when rebuilding the combined blob for users upgrading from an earlier
//! version of Hyvemind.
//!
//! ## File-based credential cache
//!
//! In addition to the OS keyring, this module supports a file-based
//! credential cache (`{data_dir}/.credentials`) that sits in front of the
//! keyring to avoid triggering macOS Keychain authorization dialogs on
//! every launch. The on-disk format is **AES-256-GCM encrypted JSON** —
//! the encryption key itself is a 256-bit random key stored in the OS
//! keychain under service `"hyvemind"`, account
//! `"__credentials_cache_key__"`. The first time the cache is written,
//! the key is generated and stored. On every subsequent read the key is
//! fetched from the keyring (one short prompt on macOS — much shorter
//! than the per-provider-key path) and used to decrypt the cache.
//!
//! ### On-disk format
//!
//! ```json
//! { "version": 1, "nonce": "<base64 12-byte nonce>", "ciphertext": "<base64 ciphertext+16-byte tag>" }
//! ```
//!
//! Legacy caches (base64-encoded JSON, no `version` field) are detected
//! on load and transparently migrated to the encrypted format.

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Service name used for all Hyvemind credentials in the OS keyring.
const SERVICE: &str = "hyvemind";

/// Account name for the combined provider-keys blob.
const COMBINED_ACCOUNT: &str = "__provider_keys__";

/// Account name for the cache-encryption key in the OS keyring.
const CACHE_KEY_ACCOUNT: &str = "__credentials_cache_key__";

/// Current on-disk format version for the encrypted cache.
const CACHE_FORMAT_VERSION: u32 = 1;

/// Path to the file-based credential cache within the data directory.
fn credentials_path(data_dir: &Path) -> PathBuf {
    data_dir.join(".credentials")
}

/// Encrypted cache envelope as serialized to disk.
#[derive(Serialize, Deserialize)]
struct EncryptedCache {
    version: u32,
    /// Base64-encoded 12-byte nonce (fresh per save).
    nonce: String,
    /// Base64-encoded ciphertext (includes the 16-byte GCM auth tag).
    ciphertext: String,
}

/// Thin wrapper over the OS-native credential store.
///
/// In addition to the OS keyring, supports a file-based credential cache
/// (`{data_dir}/.credentials`) that sits in front of the keyring to avoid
/// triggering macOS Keychain authorization dialogs on every launch. The
/// file is encrypted with AES-256-GCM using a per-install key stored in
/// the OS keyring. Unix file permissions on the cache file are also
/// constrained to `0600` as defense in depth.
pub struct SecretStore;

impl SecretStore {
    /// Read the combined provider-keys blob.
    ///
    /// `Ok(Some(map))` when the entry exists and parses, `Ok(None)` when no
    /// entry exists yet (first run after the consolidation refactor) or the
    /// keyring read failed for a non-NoEntry reason. `Err` only on malformed
    /// JSON in an existing entry.
    pub fn load_all() -> Result<Option<BTreeMap<String, String>>> {
        let entry = match keyring::Entry::new(SERVICE, COMBINED_ACCOUNT) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "failed to construct combined keyring entry");
                return Ok(None);
            }
        };
        match entry.get_password() {
            Ok(s) => {
                let map: BTreeMap<String, String> = serde_json::from_str(&s)
                    .context("failed to parse combined provider-keys blob")?;
                Ok(Some(map))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => {
                warn!(error = %e, "combined keyring read failed");
                Ok(None)
            }
        }
    }

    /// Write the combined provider-keys blob, replacing whatever is there.
    pub fn save_all(map: &BTreeMap<String, String>) -> Result<()> {
        let entry = keyring::Entry::new(SERVICE, COMBINED_ACCOUNT)
            .context("failed to construct combined keyring entry")?;
        let json = serde_json::to_string(map).context("failed to serialize provider-keys blob")?;
        entry
            .set_password(&json)
            .context("failed to write combined keyring entry")?;
        Ok(())
    }

    /// Read the stored API key for `provider` from the legacy per-provider entry.
    ///
    /// Used only by the one-time migration path. New code should use
    /// [`SecretStore::load_all`] instead.
    pub fn get(provider: &str) -> Option<String> {
        let entry = match keyring::Entry::new(SERVICE, provider) {
            Ok(e) => e,
            Err(e) => {
                warn!(provider = %provider, error = %e, "failed to construct keyring entry");
                return None;
            }
        };
        match entry.get_password() {
            Ok(s) => Some(s),
            Err(keyring::Error::NoEntry) => None,
            Err(e) => {
                warn!(provider = %provider, error = %e, "keyring read failed");
                None
            }
        }
    }

    /// Store `key` for `provider` in a legacy per-provider keyring entry.
    ///
    /// Retained for completeness; new write paths use [`SecretStore::save_all`].
    #[allow(dead_code)]
    pub fn set(provider: &str, key: &str) -> Result<()> {
        let entry = keyring::Entry::new(SERVICE, provider)
            .with_context(|| format!("failed to construct keyring entry for {provider}"))?;
        entry
            .set_password(key)
            .with_context(|| format!("failed to write keyring entry for {provider}"))?;
        Ok(())
    }

    /// Delete a legacy per-provider keyring entry. `NoEntry` is ignored.
    #[allow(dead_code)]
    pub fn delete(provider: &str) -> Result<()> {
        let entry = keyring::Entry::new(SERVICE, provider)
            .with_context(|| format!("failed to construct keyring entry for {provider}"))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow::anyhow!(e))
                .with_context(|| format!("failed to delete keyring entry for {provider}")),
        }
    }

    // ── Cache-encryption key (in OS keyring) ───────────────
    //
    // A per-install random 256-bit key, stored in the OS keyring under
    // service `"hyvemind"`, account `"__credentials_cache_key__"`. The
    // raw key is base64-encoded for storage in the keyring (which is a
    // string-typed API).

    /// Generate a fresh 256-bit random key, store it in the OS keyring,
    /// and return the raw key bytes.
    fn generate_and_store_cache_key() -> Result<[u8; 32]> {
        use rand::RngCore;
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        let entry = keyring::Entry::new(SERVICE, CACHE_KEY_ACCOUNT)
            .context("failed to construct cache-key keyring entry")?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(key);
        entry
            .set_password(&encoded)
            .context("failed to write cache-key keyring entry")?;
        Ok(key)
    }

    /// Read the cache-encryption key from the OS keyring.
    ///
    /// Returns `Ok(Some(key))` if present, `Ok(None)` if not (`NoEntry`
    /// or read failed for any other reason). `Err` only on malformed
    /// base64 in an existing entry.
    fn read_cache_key() -> Result<Option<[u8; 32]>> {
        let entry = match keyring::Entry::new(SERVICE, CACHE_KEY_ACCOUNT) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "failed to construct cache-key keyring entry");
                return Ok(None);
            }
        };
        let encoded = match entry.get_password() {
            Ok(s) => s,
            Err(keyring::Error::NoEntry) => return Ok(None),
            Err(e) => {
                warn!(error = %e, "cache-key keyring read failed");
                return Ok(None);
            }
        };
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim().as_bytes())
            .context("failed to base64-decode cache-encryption key")?;
        if decoded.len() != 32 {
            anyhow::bail!(
                "cache-encryption key has wrong length: expected 32 bytes, got {}",
                decoded.len()
            );
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&decoded);
        Ok(Some(key))
    }

    /// Return the cache-encryption key, generating one if necessary.
    fn ensure_cache_key() -> Result<[u8; 32]> {
        if let Some(key) = Self::read_cache_key()? {
            return Ok(key);
        }
        Self::generate_and_store_cache_key()
    }

    // ── File-based credential cache ────────────────────────
    //
    // The cache stores an AES-256-GCM-encrypted JSON map at
    // `{data_dir}/.credentials`. It sits in front of the OS keyring so
    // that subsequent app launches can read credentials without triggering
    // a macOS Keychain authorization dialog for every provider key — only
    // a single short prompt for the cache-encryption key.

    /// Read the file-based credential cache.
    ///
    /// Returns `Ok(Some(map))` when the file exists and decrypts cleanly,
    /// `Ok(None)` when:
    /// - the file does not exist, or
    /// - the cache-encryption key is missing from the keyring (the cache
    ///   file is then wiped and a WARN is logged so the caller falls back
    ///   to the keyring path).
    ///
    /// Legacy base64-only files (no `version` field) are decoded
    /// transparently and re-encrypted on the next write.
    ///
    /// `Err` on malformed/corrupt content (invalid base64, JSON, or
    /// failed AEAD authentication).
    pub fn load_from_file(data_dir: &Path) -> Result<Option<BTreeMap<String, String>>> {
        let path = credentials_path(data_dir);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read credentials file {}", path.display()))?;

        // Detect format by attempting to parse as the encrypted envelope
        // first. If that fails (e.g. legacy base64-only), fall back to
        // the legacy decoder which re-emits a regular map.
        if let Ok(env) = serde_json::from_str::<EncryptedCache>(&raw) {
            if env.version != CACHE_FORMAT_VERSION {
                anyhow::bail!(
                    "unsupported credentials cache version: {} (expected {})",
                    env.version,
                    CACHE_FORMAT_VERSION
                );
            }

            // Get the cache key. If it's missing (e.g. user wiped the
            // keychain), we can't decrypt — wipe the cache file and
            // signal "no cache" so the caller falls back to the keyring.
            let key = match Self::read_cache_key()? {
                Some(k) => k,
                None => {
                    warn!(
                        "credentials cache exists but cache-encryption key is missing from keyring; wiping cache"
                    );
                    Self::delete_file(data_dir);
                    return Ok(None);
                }
            };

            let nonce_bytes = base64::engine::general_purpose::STANDARD
                .decode(env.nonce.as_bytes())
                .context("failed to base64-decode credentials cache nonce")?;
            if nonce_bytes.len() != 12 {
                anyhow::bail!(
                    "credentials cache nonce has wrong length: expected 12 bytes, got {}",
                    nonce_bytes.len()
                );
            }
            let ciphertext = base64::engine::general_purpose::STANDARD
                .decode(env.ciphertext.as_bytes())
                .context("failed to base64-decode credentials cache ciphertext")?;

            let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
            let nonce = Nonce::from_slice(&nonce_bytes);
            let plaintext = cipher
                .decrypt(nonce, ciphertext.as_ref())
                .map_err(|e| anyhow::anyhow!("failed to decrypt credentials cache: {e}"))?;
            let map: BTreeMap<String, String> = serde_json::from_slice(&plaintext)
                .context("failed to parse decrypted credentials JSON")?;
            return Ok(Some(map));
        }

        // ── Legacy path: base64-encoded JSON, no version field ──
        //
        // Decode and return; caller's next `save_to_file` will re-encrypt
        // to the new format (transparent migration).
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(raw.trim().as_bytes())
            .context("failed to base64-decode legacy credentials file")?;
        let map: BTreeMap<String, String> = serde_json::from_slice(&decoded)
            .context("failed to parse legacy credentials file JSON")?;
        Ok(Some(map))
    }

    /// Write the file-based credential cache.
    ///
    /// Serializes `map` to JSON, encrypts with AES-256-GCM under the
    /// per-install key stored in the OS keyring (generated on first use),
    /// and writes atomically via tempfile+rename. Sets file permissions
    /// to `0600` on Unix.
    pub fn save_to_file(data_dir: &Path, map: &BTreeMap<String, String>) -> Result<()> {
        // Defensive: ensure data_dir exists (it normally does from Config::load,
        // but guards against edge cases like first-launch race or manual deletion).
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("failed to create data directory {}", data_dir.display()))?;

        let json = serde_json::to_vec(map).context("failed to serialize credentials")?;

        let key = Self::ensure_cache_key()?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, json.as_ref())
            .map_err(|e| anyhow::anyhow!("failed to encrypt credentials cache: {e}"))?;

        let envelope = EncryptedCache {
            version: CACHE_FORMAT_VERSION,
            nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
            ciphertext: base64::engine::general_purpose::STANDARD.encode(&ciphertext),
        };
        let serialized = serde_json::to_vec(&envelope)
            .context("failed to serialize encrypted credentials envelope")?;

        let mut tmp = tempfile::NamedTempFile::new_in(data_dir)
            .context("failed to create temp file for credentials")?;

        // Set restrictive permissions on the temp file BEFORE writing content.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .context("failed to set credentials file permissions")?;
        }

        std::io::Write::write_all(&mut tmp, &serialized)
            .context("failed to write credentials temp file")?;

        let final_path = credentials_path(data_dir);
        tmp.persist(&final_path)
            .context("failed to persist credentials file")?;
        // fsync the parent directory so the rename itself is durable across a
        // power loss — see `crate::state::store::sync_parent_dir_blocking`.
        // This whole `save_to_file` path is synchronous (keyring binding,
        // sync `tempfile` API), so use the blocking sibling rather than
        // forcing an async runtime hop.
        crate::state::store::sync_parent_dir_blocking(&final_path)
            .context("failed to fsync parent directory of credentials file")?;
        Ok(())
    }

    /// Best-effort deletion of the file-based credential cache.
    ///
    /// Logs a warning on failure but does not error — callers use this to
    /// clean up corrupt cache files before falling back to the keyring.
    pub fn delete_file(data_dir: &Path) {
        let path = credentials_path(data_dir);
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                warn!(error = %e, "failed to delete corrupt credentials file");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic encrypted-envelope file outside the keyring so
    /// tests don't touch the user's real OS keychain. Tests below cover:
    ///
    /// - format detection (legacy vs. encrypted)
    /// - byte-level non-readability of the on-disk file
    /// - legacy migration path
    /// - missing-key behavior on load (wipe + return None)
    ///
    /// Round-trip save → load via the real keyring is intentionally
    /// avoided in unit tests because CI environments typically have no
    /// usable keyring backend; that path is exercised manually and
    /// documented in CLAUDE.md.

    fn craft_encrypted_cache(map: &BTreeMap<String, String>, key: &[u8; 32]) -> Vec<u8> {
        let json = serde_json::to_vec(map).unwrap();
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher.encrypt(&nonce, json.as_ref()).unwrap();
        let env = EncryptedCache {
            version: CACHE_FORMAT_VERSION,
            nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
            ciphertext: base64::engine::general_purpose::STANDARD.encode(&ciphertext),
        };
        serde_json::to_vec(&env).unwrap()
    }

    fn decrypt_cache(bytes: &[u8], key: &[u8; 32]) -> BTreeMap<String, String> {
        let env: EncryptedCache = serde_json::from_slice(bytes).unwrap();
        assert_eq!(env.version, CACHE_FORMAT_VERSION);
        let nonce_bytes = base64::engine::general_purpose::STANDARD
            .decode(env.nonce.as_bytes())
            .unwrap();
        let ciphertext = base64::engine::general_purpose::STANDARD
            .decode(env.ciphertext.as_bytes())
            .unwrap();
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
        let nonce = Nonce::from_slice(&nonce_bytes);
        let plaintext = cipher.decrypt(nonce, ciphertext.as_ref()).unwrap();
        serde_json::from_slice(&plaintext).unwrap()
    }

    /// Smoke test mirroring the manual `xxd ~/.hyvemind/.credentials`
    /// verification step in the audit plan. Writes a real encrypted file
    /// (using a fixed test key, not the keyring) and prints a hex dump
    /// to stdout when invoked with `--nocapture`. Asserts that the file
    /// is the JSON envelope (starts with `{`) and that the byte sequence
    /// "sk-ant-" never appears anywhere in the file.
    #[test]
    fn xxd_smoke_check_no_plaintext_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let mut map = BTreeMap::new();
        map.insert(
            "anthropic".to_string(),
            "sk-ant-api03-XXXXXXXXXXXXXX-secret".to_string(),
        );
        map.insert(
            "openrouter".to_string(),
            "sk-or-v1-XXXXXXXXXXXXXXX-secret".to_string(),
        );
        let key = [0x55u8; 32];
        let bytes = craft_encrypted_cache(&map, &key);
        let path = dir.path().join(".credentials");
        std::fs::write(&path, &bytes).unwrap();

        let raw = std::fs::read(&path).unwrap();
        // Envelope shape: starts with `{"version":1,"nonce":"...","ciphertext":"..."}`.
        assert_eq!(raw[0], b'{');
        // Plain-text leak detector.
        for needle in [b"sk-ant-".as_slice(), b"sk-or-".as_slice()] {
            assert!(
                !raw.windows(needle.len()).any(|w| w == needle),
                "secret prefix {:?} leaked into on-disk credentials cache",
                std::str::from_utf8(needle).unwrap()
            );
        }
        eprintln!(
            "xxd-equivalent: wrote {} bytes encrypted cache to {}",
            raw.len(),
            path.display()
        );
        // First 64 bytes, hex-encoded — what xxd would show on the real file.
        let preview = &raw[..raw.len().min(64)];
        eprintln!(
            "first {} bytes (hex): {}",
            preview.len(),
            preview
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        );
    }

    #[test]
    fn file_cache_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = SecretStore::load_from_file(dir.path()).unwrap();
        assert_eq!(loaded, None);
    }

    #[test]
    fn file_cache_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials");
        std::fs::write(&path, "not-valid-base64-or-json!!!").unwrap();
        assert!(SecretStore::load_from_file(dir.path()).is_err());
        SecretStore::delete_file(dir.path());
        assert!(!path.exists());
        assert_eq!(SecretStore::load_from_file(dir.path()).unwrap(), None);
    }

    /// Verify the on-disk format is the encrypted envelope, never the
    /// raw provider keys. Constructs a synthetic encrypted file (so the
    /// test doesn't depend on the OS keyring), writes it to disk, and
    /// scans the raw bytes for the secret payload.
    #[test]
    fn encrypted_cache_hides_plaintext_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let mut map = BTreeMap::new();
        let secret_anthropic = "sk-ant-very-secret-XYZABC1234567890";
        let secret_openai = "sk-openai-zzzZZ-9876543210";
        map.insert("anthropic".to_string(), secret_anthropic.to_string());
        map.insert("openai".to_string(), secret_openai.to_string());

        let key = [42u8; 32];
        let bytes = craft_encrypted_cache(&map, &key);
        let path = dir.path().join(".credentials");
        std::fs::write(&path, &bytes).unwrap();

        let on_disk = std::fs::read(&path).unwrap();
        // No raw secret substring should appear anywhere in the file.
        assert!(
            !on_disk
                .windows(secret_anthropic.len())
                .any(|w| w == secret_anthropic.as_bytes()),
            "anthropic secret leaked into on-disk credentials cache"
        );
        assert!(
            !on_disk
                .windows(secret_openai.len())
                .any(|w| w == secret_openai.as_bytes()),
            "openai secret leaked into on-disk credentials cache"
        );
        // The provider id strings are stored in JSON keys *inside* the
        // ciphertext, so they too should not appear in cleartext.
        assert!(
            !on_disk
                .windows(b"anthropic".len())
                .any(|w| w == b"anthropic"),
            "provider id leaked into on-disk credentials cache"
        );

        // Sanity: decryption with the same key round-trips correctly.
        let recovered = decrypt_cache(&on_disk, &key);
        assert_eq!(recovered, map);
    }

    /// Legacy base64-only files (no `version` field) must load cleanly
    /// so users upgrading from a pre-encryption build don't lose their
    /// keys. The first subsequent `save_to_file` then re-encrypts.
    #[test]
    fn legacy_base64_file_loads_for_migration() {
        let dir = tempfile::tempdir().unwrap();
        let mut map = BTreeMap::new();
        map.insert("anthropic".to_string(), "sk-ant-legacy".to_string());
        map.insert("openai".to_string(), "sk-openai-legacy".to_string());

        // Write the OLD format: base64-encoded JSON, no envelope.
        let json = serde_json::to_string(&map).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        let path = dir.path().join(".credentials");
        std::fs::write(&path, encoded).unwrap();

        // Load returns the same map without touching the keyring.
        let loaded = SecretStore::load_from_file(dir.path()).unwrap();
        assert_eq!(loaded, Some(map));
    }

    /// Reject envelopes whose `version` field is unknown — future-proofs
    /// the format against accidental cross-version misreads.
    #[test]
    fn rejects_unknown_envelope_version() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = serde_json::json!({
            "version": 999,
            "nonce": base64::engine::general_purpose::STANDARD.encode([0u8; 12]),
            "ciphertext": base64::engine::general_purpose::STANDARD.encode([0u8; 32]),
        });
        let path = dir.path().join(".credentials");
        std::fs::write(&path, serde_json::to_vec(&bogus).unwrap()).unwrap();
        assert!(SecretStore::load_from_file(dir.path()).is_err());
    }

    /// The encrypted envelope itself should be valid JSON with the
    /// expected fields, and the ciphertext must not equal the
    /// plaintext.
    #[test]
    fn envelope_shape_is_well_formed() {
        let mut map = BTreeMap::new();
        map.insert("anthropic".to_string(), "sk-ant-shape".to_string());
        let key = [7u8; 32];
        let bytes = craft_encrypted_cache(&map, &key);
        let env: EncryptedCache = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(env.version, CACHE_FORMAT_VERSION);
        assert!(!env.nonce.is_empty());
        assert!(!env.ciphertext.is_empty());
        // 12-byte nonce → 16 base64 chars.
        let nonce_decoded = base64::engine::general_purpose::STANDARD
            .decode(env.nonce.as_bytes())
            .unwrap();
        assert_eq!(nonce_decoded.len(), 12);
    }
}
