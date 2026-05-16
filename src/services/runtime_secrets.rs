use crate::models::runtime_env_vars::{
    ProjectRuntimeEncryptionKeyInsert, ProjectRuntimeEncryptionKeyRow,
};
use crate::schema::project_runtime_encryption_keys;
use base64::Engine;
use diesel::prelude::*;
use diesel::result::Error as DieselError;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::rand::{SecureRandom, SystemRandom};
use thiserror::Error;
use uuid::Uuid;

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

#[derive(Debug, Error)]
pub enum RuntimeSecretsError {
    #[error("invalid runtime encryption key")]
    InvalidKey,
    #[error("encryption failed")]
    EncryptFailed,
    #[error("decryption failed")]
    DecryptFailed,
    #[error("database error")]
    Database(#[from] DieselError),
}

pub fn generate_runtime_encryption_key() -> Result<[u8; KEY_LEN], RuntimeSecretsError> {
    let mut key = [0u8; KEY_LEN];
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| RuntimeSecretsError::EncryptFailed)?;
    Ok(key)
}

fn bytes_to_key(bytes: &[u8]) -> Result<[u8; KEY_LEN], RuntimeSecretsError> {
    if bytes.len() != KEY_LEN {
        return Err(RuntimeSecretsError::InvalidKey);
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(bytes);
    Ok(key)
}

pub fn ensure_runtime_encryption_key(
    conn: &mut diesel::PgConnection,
    runtime_id: Uuid,
) -> Result<ProjectRuntimeEncryptionKeyRow, RuntimeSecretsError> {
    if let Some(row) = project_runtime_encryption_keys::table
        .filter(project_runtime_encryption_keys::runtime_id.eq(runtime_id))
        .select(ProjectRuntimeEncryptionKeyRow::as_select())
        .first(conn)
        .optional()?
    {
        return Ok(row);
    }

    let encryption_key = generate_runtime_encryption_key()?.to_vec();
    diesel::insert_into(project_runtime_encryption_keys::table)
        .values(&ProjectRuntimeEncryptionKeyInsert {
            runtime_id,
            encryption_key,
        })
        .returning(ProjectRuntimeEncryptionKeyRow::as_returning())
        .get_result(conn)
        .map_err(RuntimeSecretsError::from)
}

pub fn insert_runtime_encryption_keys_for_runtimes(
    conn: &mut diesel::PgConnection,
    runtime_ids: &[Uuid],
) -> Result<(), RuntimeSecretsError> {
    for runtime_id in runtime_ids {
        ensure_runtime_encryption_key(conn, *runtime_id)?;
    }
    Ok(())
}

pub fn encrypt_runtime_env_value(
    runtime_key: &[u8; KEY_LEN],
    plaintext: &str,
) -> Result<String, RuntimeSecretsError> {
    let unbound = UnboundKey::new(&AES_256_GCM, runtime_key)
        .map_err(|_| RuntimeSecretsError::EncryptFailed)?;
    let key = LessSafeKey::new(unbound);

    let mut nonce_bytes = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| RuntimeSecretsError::EncryptFailed)?;
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);

    let mut in_out = plaintext.as_bytes().to_vec();
    key.seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| RuntimeSecretsError::EncryptFailed)?;

    let mut blob = nonce_bytes.to_vec();
    blob.extend(in_out);
    Ok(base64::engine::general_purpose::STANDARD.encode(blob))
}

pub fn decrypt_runtime_env_value(
    runtime_key: &[u8; KEY_LEN],
    stored: &str,
) -> Result<String, RuntimeSecretsError> {
    let blob = base64::engine::general_purpose::STANDARD
        .decode(stored.trim())
        .map_err(|_| RuntimeSecretsError::DecryptFailed)?;
    if blob.len() <= NONCE_LEN {
        return Err(RuntimeSecretsError::DecryptFailed);
    }

    let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
    let mut nonce_arr = [0u8; NONCE_LEN];
    nonce_arr.copy_from_slice(nonce_bytes);
    let nonce = Nonce::assume_unique_for_key(nonce_arr);

    let unbound = UnboundKey::new(&AES_256_GCM, runtime_key)
        .map_err(|_| RuntimeSecretsError::DecryptFailed)?;
    let key = LessSafeKey::new(unbound);

    let mut in_out = ciphertext.to_vec();
    let plain = key
        .open_in_place(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| RuntimeSecretsError::DecryptFailed)?;
    String::from_utf8(plain.to_vec()).map_err(|_| RuntimeSecretsError::DecryptFailed)
}

pub fn prepare_stored_value(
    runtime_key: &[u8; KEY_LEN],
    plaintext: &str,
    is_encrypted: bool,
) -> Result<String, RuntimeSecretsError> {
    if plaintext.is_empty() {
        return Ok(String::new());
    }
    if is_encrypted {
        encrypt_runtime_env_value(runtime_key, plaintext)
    } else {
        Ok(plaintext.to_string())
    }
}

pub fn runtime_key_from_row(row: &ProjectRuntimeEncryptionKeyRow) -> Result<[u8; KEY_LEN], RuntimeSecretsError> {
    bytes_to_key(&row.encryption_key)
}
