//! Signer construction.
//!
//! Two signer sources are supported in this slice:
//!
//! - Raw private key from `SIGNER_PRIVATE_KEY` (or the legacy
//!   `EXECUTOR_PRIVATE_KEY`), expected as 0x-prefixed 32-byte hex.
//! - Encrypted JSON keystore on disk, decrypted with the password from
//!   `SIGNER_KEYSTORE_PASSWORD`.
//!
//! AWS KMS support is not implemented yet; the config layer accepts the key
//! id so deployment topology can stage it, but [`load_signer`] returns an
//! error when KMS is the only configured source.

use std::path::Path;

use alloy_primitives::Address as AlloyAddress;
use alloy_signer::Signer;
use alloy_signer_local::{LocalSigner, PrivateKeySigner};
use anyhow::{anyhow, Context};
use config::SignerConfig;

#[derive(Debug, Clone)]
pub struct EngineSigner {
    inner: PrivateKeySigner,
}

impl EngineSigner {
    pub fn address(&self) -> AlloyAddress {
        self.inner.address()
    }

    pub fn into_inner(self) -> PrivateKeySigner {
        self.inner
    }

    pub fn inner(&self) -> &PrivateKeySigner {
        &self.inner
    }
}

pub fn load_signer(config: &SignerConfig, chain_id: u64) -> anyhow::Result<EngineSigner> {
    let mut signer = if let Some(raw) = config.private_key.as_deref() {
        signer_from_private_key(raw)?
    } else if let Some(path) = config.keystore_path.as_deref() {
        let password = config.keystore_password.as_deref().ok_or_else(|| {
            anyhow!(
                "SIGNER_KEYSTORE_PATH is set but SIGNER_KEYSTORE_PASSWORD is missing"
            )
        })?;
        signer_from_keystore(Path::new(path), password)?
    } else if config.aws_kms_key_id.is_some() {
        anyhow::bail!(
            "AWS KMS signer is configured but not yet implemented; provide SIGNER_PRIVATE_KEY or SIGNER_KEYSTORE_PATH instead"
        );
    } else {
        anyhow::bail!("no signer source configured");
    };

    signer.set_chain_id(Some(chain_id));
    Ok(EngineSigner { inner: signer })
}

fn signer_from_private_key(raw: &str) -> anyhow::Result<PrivateKeySigner> {
    let trimmed = raw.trim().trim_start_matches("0x");
    let bytes = hex_decode_32(trimmed)
        .context("SIGNER_PRIVATE_KEY must be 32-byte hex (with or without 0x prefix)")?;
    PrivateKeySigner::from_bytes(&bytes.into())
        .context("failed to construct signer from SIGNER_PRIVATE_KEY")
}

fn signer_from_keystore(path: &Path, password: &str) -> anyhow::Result<PrivateKeySigner> {
    LocalSigner::decrypt_keystore(path, password)
        .with_context(|| format!("failed to decrypt keystore at {}", path.display()))
}

fn hex_decode_32(input: &str) -> anyhow::Result<[u8; 32]> {
    if input.len() != 64 {
        anyhow::bail!("expected 64 hex characters, got {}", input.len());
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let start = i * 2;
        let end = start + 2;
        *byte = u8::from_str_radix(&input[start..end], 16)
            .with_context(|| format!("invalid hex byte at offset {start}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::SignerConfig;

    fn cfg(pk: Option<&str>) -> SignerConfig {
        SignerConfig {
            private_key: pk.map(ToString::to_string),
            keystore_path: None,
            keystore_password: None,
            aws_kms_key_id: None,
        }
    }

    #[test]
    fn loads_signer_from_private_key() {
        let pk = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        let signer = load_signer(&cfg(Some(pk)), 8453).unwrap();
        assert_eq!(format!("{:?}", signer.address()).to_lowercase().len(), 42);
    }

    #[test]
    fn rejects_short_private_key() {
        let error = load_signer(&cfg(Some("0xabc")), 8453).unwrap_err();
        let chained: String = error.chain().map(|e| e.to_string()).collect::<Vec<_>>().join(" | ");
        assert!(
            chained.contains("64 hex"),
            "expected error chain to mention `64 hex`, got: {chained}"
        );
    }

    #[test]
    fn errors_when_no_source_provided() {
        let error = load_signer(&cfg(None), 8453).unwrap_err();
        assert!(error.to_string().contains("no signer source"));
    }
}
