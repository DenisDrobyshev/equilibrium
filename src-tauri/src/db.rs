//! Encrypted local store.
//!
//! Everything the user says lives here and nowhere else.
//!
//! The database is held in memory and persisted as a single encrypted blob
//! (XChaCha20-Poly1305, key derived from the user's passphrase with Argon2id).
//! There is no moment at which an unencrypted database file exists on disk —
//! which is a stronger guarantee than SQLCipher gives, since SQLCipher writes
//! decrypted pages into a real file. It also keeps the build free of OpenSSL
//! and any other system dependency, so the project stays buildable from a
//! clean checkout on all three platforms.
//!
//! The trade-off is that the whole database lives in RAM. For this workload
//! that is fine: a year of practice transcripts is tens of megabytes.
//!
//! The passphrase is never stored. Losing it means losing the data — that is
//! the intended behaviour, not an oversight.

use anyhow::{bail, Context, Result};
use argon2::Argon2;
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use rusqlite::{Connection, MAIN_DB};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const VAULT_FILE: &str = "equilibrium.vault";
const MAGIC: &[u8; 4] = b"EQLB";
const FORMAT_VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
const HEADER_LEN: usize = 4 + 1 + SALT_LEN + NONCE_LEN;

const SCHEMA_V1: &str = include_str!("../migrations/001_init.sql");

pub struct Store {
    conn: Connection,
    vault_path: PathBuf,
    salt: [u8; SALT_LEN],
    key: [u8; KEY_LEN],
}

impl Store {
    /// Opens the vault in `data_dir`, creating it on first run.
    ///
    /// A wrong passphrase fails authentication on the AEAD tag, so it is
    /// reported as such rather than silently producing an empty database.
    pub fn open(data_dir: &Path, passphrase: &str) -> Result<Self> {
        fs::create_dir_all(data_dir).context("creating data directory")?;
        let vault_path = data_dir.join(VAULT_FILE);

        let store = if vault_path.exists() {
            Self::open_existing(&vault_path, passphrase)?
        } else {
            Self::create_new(&vault_path, passphrase)?
        };

        store.conn.pragma_update(None, "foreign_keys", true)?;
        Ok(store)
    }

    fn create_new(vault_path: &Path, passphrase: &str) -> Result<Self> {
        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).context("generating key salt")?;
        let key = derive_key(passphrase, &salt)?;

        let conn = Connection::open_in_memory().context("creating in-memory database")?;
        conn.execute_batch(SCHEMA_V1).context("applying schema v1")?;

        let store = Store {
            conn,
            vault_path: vault_path.to_path_buf(),
            salt,
            key,
        };
        store.save().context("writing the new vault")?;
        Ok(store)
    }

    fn open_existing(vault_path: &Path, passphrase: &str) -> Result<Self> {
        let raw = fs::read(vault_path).context("reading vault file")?;
        if raw.len() < HEADER_LEN {
            bail!("vault file is truncated");
        }
        if &raw[..4] != MAGIC {
            bail!("not an Equilibrium vault file");
        }
        if raw[4] != FORMAT_VERSION {
            bail!("unsupported vault format version: {}", raw[4]);
        }

        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&raw[5..5 + SALT_LEN]);
        let mut nonce_bytes = [0u8; NONCE_LEN];
        nonce_bytes.copy_from_slice(&raw[5 + SALT_LEN..HEADER_LEN]);
        let nonce = XNonce::from(nonce_bytes);
        let ciphertext = &raw[HEADER_LEN..];

        let key = derive_key(passphrase, &salt)?;
        let cipher = XChaCha20Poly1305::new((&key).into());
        let plaintext = cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|_| anyhow::anyhow!("wrong passphrase, or the vault has been tampered with"))?;

        let mut conn = Connection::open_in_memory().context("creating in-memory database")?;
        let size = plaintext.len();
        conn.deserialize_read_exact(MAIN_DB, Cursor::new(&plaintext), size, false)
            .context("loading database from vault")?;

        let store = Store {
            conn,
            vault_path: vault_path.to_path_buf(),
            salt,
            key,
        };
        store.migrate()?;
        Ok(store)
    }

    /// Serializes, encrypts and atomically replaces the vault on disk.
    ///
    /// Called after every state transition that produces data, not on a timer:
    /// losing a practice to a crash is not acceptable, and the volume is small
    /// enough that saving eagerly costs nothing.
    pub fn save(&self) -> Result<()> {
        let data = self
            .conn
            .serialize(MAIN_DB)
            .context("serializing database")?;

        let mut nonce_bytes = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce_bytes).context("generating nonce")?;
        let nonce = XNonce::from(nonce_bytes);

        let cipher = XChaCha20Poly1305::new((&self.key).into());
        let ciphertext = cipher
            .encrypt(&nonce, &*data)
            .map_err(|_| anyhow::anyhow!("encrypting vault"))?;

        let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
        out.extend_from_slice(MAGIC);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);

        // Write to a sibling temp file, then rename: a crash mid-write must not
        // destroy the previous vault.
        let tmp = self.vault_path.with_extension("vault.tmp");
        fs::write(&tmp, &out).context("writing temporary vault")?;
        fs::rename(&tmp, &self.vault_path).context("replacing vault")?;
        Ok(())
    }

    fn migrate(&self) -> Result<()> {
        if self.schema_version()? < 1 {
            self.conn
                .execute_batch(SCHEMA_V1)
                .context("applying schema v1")?;
        }
        Ok(())
    }

    fn schema_version(&self) -> Result<i64> {
        let has_meta: i64 = self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |row| row.get(0),
        )?;
        if has_meta == 0 {
            return Ok(0);
        }
        let version: String = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0))
            .unwrap_or_else(|_| "0".to_string());
        Ok(version.parse().unwrap_or(0))
    }

    /// Removes the vault permanently. Because the database never touches disk
    /// unencrypted and derived artifacts live inside it, deleting this one file
    /// is a complete erasure — nothing survives in an index or cache elsewhere.
    pub fn erase(data_dir: &Path) -> Result<()> {
        for name in [VAULT_FILE, "equilibrium.vault.tmp"] {
            let path = data_dir.join(name);
            if path.exists() {
                fs::remove_file(&path).with_context(|| format!("removing {name}"))?;
            }
        }
        Ok(())
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.key.zeroize();
    }
}

/// Argon2id with default parameters. The salt sits in the vault header in the
/// clear, which is correct: it defends against precomputation, not disclosure.
fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("deriving key: {e}"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("equilibrium-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn survives_a_round_trip_and_rejects_a_wrong_passphrase() {
        let dir = temp_dir("roundtrip");

        {
            let store = Store::open(&dir, "correct horse battery staple").unwrap();
            store
                .connection()
                .execute(
                    "INSERT INTO problems (formulation, created_at) VALUES (?1, ?2)",
                    ("по вечерам не могу заставить себя выйти из дома", "2026-08-28T19:00:00Z"),
                )
                .unwrap();
            store.save().unwrap();
        }

        // Nothing readable should be sitting on disk.
        let raw = fs::read(dir.join(VAULT_FILE)).unwrap();
        assert!(
            !String::from_utf8_lossy(&raw).contains("выйти из дома"),
            "plaintext leaked into the vault file"
        );

        {
            let store = Store::open(&dir, "correct horse battery staple").unwrap();
            let text: String = store
                .connection()
                .query_row("SELECT formulation FROM problems", [], |r| r.get(0))
                .unwrap();
            assert_eq!(text, "по вечерам не могу заставить себя выйти из дома");
        }

        assert!(
            Store::open(&dir, "wrong passphrase").is_err(),
            "a wrong passphrase must fail, not open an empty database"
        );

        Store::erase(&dir).unwrap();
        assert!(!dir.join(VAULT_FILE).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn applies_the_whole_schema() {
        let dir = temp_dir("schema");
        let store = Store::open(&dir, "pass").unwrap();
        let tables: i64 = store
            .connection()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(tables >= 15, "expected the full schema, got {tables} tables");

        // safety_events must not have a column that could hold message text.
        let cols: Vec<String> = store
            .connection()
            .prepare("SELECT name FROM pragma_table_info('safety_events')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            !cols.iter().any(|c| c == "content" || c == "message"),
            "safety_events must never store message content"
        );

        drop(store);
        Store::erase(&dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
