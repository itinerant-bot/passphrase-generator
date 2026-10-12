use serde::{de::DeserializeOwned, Serialize};
use sha3::{Digest as _, Sha3_256};
use std::fmt;
use thiserror::Error;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    #[must_use]
    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        out
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[derive(Debug, Error)]
pub enum CanonicalError {
    #[error("INVALID_JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("NONCANONICAL_JSON")]
    Noncanonical,
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    // Converting through Value recursively places object members in
    // serde_json's sorted Map (the preserve_order feature is not enabled).
    Ok(serde_json::to_vec(&serde_json::to_value(value)?)?)
}

pub fn decode_canonical_json<T>(bytes: &[u8]) -> Result<T, CanonicalError>
where
    T: DeserializeOwned + Serialize,
{
    let value: T = serde_json::from_slice(bytes)?;
    if canonical_json(&value)? != bytes {
        return Err(CanonicalError::Noncanonical);
    }
    Ok(value)
}

#[must_use]
pub fn sha3_256(bytes: &[u8]) -> Digest {
    let mut hasher = Sha3_256::new();
    hasher.update(bytes);
    Digest(hasher.finalize().into())
}
