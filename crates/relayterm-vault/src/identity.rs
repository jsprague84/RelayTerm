//! High-level identity-creation API.
//!
//! `VaultService` is what the API layer talks to: hand it an owner-supplied
//! key type, get back a public-key-shaped record plus an opaque ciphertext
//! suitable for `INSERT INTO ssh_identities`. The plaintext private key is
//! never returned to callers.

use std::fmt;

use rand::rngs::OsRng;
use ssh_key::{Algorithm, EcdsaCurve, HashAlg, LineEnding, PrivateKey};
use zeroize::Zeroizing;

use relayterm_core::ssh_identity::SshKeyType;

use crate::VaultError;
use crate::cipher::{self, EncryptedBlob};
use crate::master_key::VaultMasterKey;

/// Result of generating a new SSH identity.
///
/// The struct exposes only data that is safe to persist, return to the
/// caller, or surface in API responses. The plaintext private key never
/// leaves [`VaultService::generate_ssh_identity`] — only its encrypted form.
#[derive(Clone)]
pub struct GeneratedSshIdentity {
    /// Algorithm used for the keypair.
    pub key_type: SshKeyType,
    /// OpenSSH-format public key bytes (the `authorized_keys` line as
    /// ASCII, without trailing newline). Safe to expose.
    pub public_key_openssh: Vec<u8>,
    /// SHA-256 fingerprint of the public key, matching the
    /// `SHA256:<base64>` format `ssh-keygen -lf` emits.
    pub fingerprint_sha256: String,
    /// Encrypted private-key blob. Treat as opaque; persist as-is.
    pub encrypted_private_key: EncryptedBlob,
}

impl fmt::Debug for GeneratedSshIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratedSshIdentity")
            .field("key_type", &self.key_type)
            .field("public_key_len", &self.public_key_openssh.len())
            .field("fingerprint_sha256", &self.fingerprint_sha256)
            .field("encrypted_private_key", &self.encrypted_private_key)
            .finish()
    }
}

/// Vault service: holds the master key, generates and decrypts SSH
/// identities. Cheap to clone — the inner key is `Clone` and zeroizing.
/// Axum clones `AppState` per request, so multiple copies of the master
/// key live in memory at runtime; every copy wipes itself on drop.
#[derive(Clone)]
pub struct VaultService {
    master_key: VaultMasterKey,
}

impl fmt::Debug for VaultService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultService")
            .field("master_key", &self.master_key)
            .finish()
    }
}

impl VaultService {
    /// Construct a vault service bound to the given master key.
    #[must_use]
    pub fn new(master_key: VaultMasterKey) -> Self {
        Self { master_key }
    }

    /// Generate a new keypair of the requested type and return its public
    /// material plus the encrypted private blob.
    ///
    /// `comment` is baked into the OpenSSH key as its trailing comment, so
    /// the `authorized_keys` line a user installs is self-identifying
    /// (`ssh-ed25519 AAAA... <comment>`). Pass an empty string to omit.
    ///
    /// Currently only [`SshKeyType::Ed25519`] is supported; other variants
    /// return [`VaultError::UnsupportedKeyType`] so callers can surface a
    /// clean 400 without having to enumerate vault internals.
    pub fn generate_ssh_identity(
        &self,
        key_type: SshKeyType,
        comment: &str,
    ) -> Result<GeneratedSshIdentity, VaultError> {
        let algorithm = match key_type {
            SshKeyType::Ed25519 => Algorithm::Ed25519,
            other => return Err(VaultError::UnsupportedKeyType(other.as_str())),
        };

        let mut private =
            PrivateKey::random(&mut OsRng, algorithm).map_err(|_| VaultError::KeyGeneration)?;
        if !comment.is_empty() {
            private.set_comment(comment);
        }

        // OpenSSH `authorized_keys` line, ASCII text, no trailing newline.
        let public_key_openssh = private
            .public_key()
            .to_openssh()
            .map_err(|_| VaultError::PublicKeySerialization)?
            .into_bytes();

        // `SHA256:<base64>` fingerprint, exactly what `ssh-keygen -lf` prints.
        let fingerprint_sha256 = private
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();

        // OpenSSH PEM private key — held in a zeroizing buffer so the
        // intermediate plaintext is wiped before this function returns.
        let pem = private
            .to_openssh(LineEnding::LF)
            .map_err(|_| VaultError::PrivateKeySerialization)?;
        let plaintext: Zeroizing<Vec<u8>> = Zeroizing::new(pem.as_bytes().to_vec());

        let encrypted_private_key = cipher::encrypt(&self.master_key, &plaintext)?;

        Ok(GeneratedSshIdentity {
            key_type,
            public_key_openssh,
            fingerprint_sha256,
            encrypted_private_key,
        })
    }

    /// Import an existing OpenSSH-format private key into the vault.
    ///
    /// `pem` is the ASCII OpenSSH PEM bytes the caller supplied (the DTO
    /// layer has already pre-checked the header sentinel and ASCII bound).
    /// `name` is the trimmed identity name; baked into the OpenSSH comment
    /// of the canonical re-serialized PEM so the operator-visible identity
    /// name matches the `authorized_keys` line, mirroring the generate
    /// path. Pass an empty string to omit the comment.
    ///
    /// **Security invariants** (security-critical — read alongside
    /// `docs/private-key-import.md` § 2):
    ///
    /// - The plaintext key bytes never leave this function. The supplied
    ///   `pem` slice is dropped at function return; the canonical
    ///   re-serialized PEM is held in a `Zeroizing<Vec<u8>>` and wiped
    ///   before [`Self::import_ssh_identity`] returns.
    /// - The only durable form is the [`EncryptedBlob`] envelope on the
    ///   returned [`GeneratedSshIdentity`]; an imported key is
    ///   indistinguishable at rest from a generated key.
    /// - Errors carry no input bytes. Encrypted / malformed inputs collapse
    ///   to [`VaultError::UnsupportedFormat`] with a closed `reason`
    ///   discriminant; non-Ed25519 algorithms collapse to
    ///   [`VaultError::UnsupportedKeyType`] with a static algorithm tag —
    ///   never the parsed PEM, never the parser's error text.
    pub fn import_ssh_identity(
        &self,
        pem: &[u8],
        name: &str,
    ) -> Result<GeneratedSshIdentity, VaultError> {
        // Reject non-ASCII bytes early. The DTO layer already enforces this,
        // but defending in depth here means a future direct caller (e.g. a
        // service-internal import path) cannot bypass the rule.
        let pem_str = std::str::from_utf8(pem).map_err(|_| VaultError::UnsupportedFormat {
            reason: "malformed",
        })?;
        let mut private =
            PrivateKey::from_openssh(pem_str).map_err(|_| VaultError::UnsupportedFormat {
                reason: "malformed",
            })?;
        // Detect passphrase-protected keys explicitly. Without the
        // `encryption` cipher feature on `ssh-key`, attempting to use the
        // private bytes would fail later with a less-specific error; the
        // explicit `is_encrypted()` check produces a typed reason.
        if private.is_encrypted() {
            return Err(VaultError::UnsupportedFormat {
                reason: "encrypted",
            });
        }
        let key_type = match private.algorithm() {
            Algorithm::Ed25519 => SshKeyType::Ed25519,
            // Map known but unsupported algorithms to canonical
            // `SshKeyType` tags so the route-layer wire message
            // (`unsupported key_type "<tag>"`) matches the existing
            // generate-path shape byte-for-byte. `Algorithm` is
            // `#[non_exhaustive]`, so the catch-all is required and
            // collapses anything else to the `"unknown"` discriminant.
            Algorithm::Rsa { .. } => return Err(VaultError::UnsupportedKeyType("rsa")),
            Algorithm::Dsa => return Err(VaultError::UnsupportedKeyType("dsa")),
            Algorithm::Ecdsa { curve } => {
                return Err(VaultError::UnsupportedKeyType(match curve {
                    EcdsaCurve::NistP256 => "ecdsa_p256",
                    EcdsaCurve::NistP384 => "ecdsa_p384",
                    EcdsaCurve::NistP521 => "ecdsa_p521",
                }));
            }
            _ => return Err(VaultError::UnsupportedKeyType("unknown")),
        };

        if !name.is_empty() {
            private.set_comment(name);
        }

        let public_key_openssh = private
            .public_key()
            .to_openssh()
            .map_err(|_| VaultError::PublicKeySerialization)?
            .into_bytes();

        let fingerprint_sha256 = private
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();

        // `to_openssh` already returns `Zeroizing<String>`; copy into the
        // bytes form the AEAD wants and let both buffers wipe on drop.
        let canonical_pem = private
            .to_openssh(LineEnding::LF)
            .map_err(|_| VaultError::PrivateKeySerialization)?;
        let plaintext: Zeroizing<Vec<u8>> = Zeroizing::new(canonical_pem.as_bytes().to_vec());

        let encrypted_private_key = cipher::encrypt(&self.master_key, &plaintext)?;

        Ok(GeneratedSshIdentity {
            key_type,
            public_key_openssh,
            fingerprint_sha256,
            encrypted_private_key,
        })
    }

    /// Decrypt a stored ciphertext into the plaintext OpenSSH private-key
    /// PEM. The buffer wipes itself on drop.
    ///
    /// Reachable from tests and the (future) SSH session task; never
    /// surfaced through an HTTP handler.
    pub fn decrypt_private_key(&self, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        cipher::decrypt(&self.master_key, blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use sha2::{Digest, Sha256};

    fn sha256(input: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(input);
        hasher.finalize().into()
    }

    fn vault() -> VaultService {
        VaultService::new(VaultMasterKey::from_bytes([0x91u8; 32]))
    }

    #[test]
    fn generates_ed25519_with_openssh_public_key() {
        let identity = vault()
            .generate_ssh_identity(SshKeyType::Ed25519, "")
            .unwrap();
        let public = std::str::from_utf8(&identity.public_key_openssh).unwrap();
        assert!(
            public.starts_with("ssh-ed25519 "),
            "expected OpenSSH ed25519 public key prefix, got: {public}",
        );
        // No trailing newline — `authorized_keys` lines are appended with
        // their own newline by callers if needed.
        assert!(!public.ends_with('\n'));
    }

    #[test]
    fn fingerprint_uses_sha256_format() {
        let identity = vault()
            .generate_ssh_identity(SshKeyType::Ed25519, "")
            .unwrap();
        assert!(
            identity.fingerprint_sha256.starts_with("SHA256:"),
            "expected SHA256: prefix, got: {}",
            identity.fingerprint_sha256
        );
        // Trailing portion is base64 of the 32-byte digest, so 43 chars
        // (no padding) plus the prefix.
        assert!(identity.fingerprint_sha256.len() >= "SHA256:".len() + 32);
    }

    #[test]
    fn fingerprint_matches_independent_sha256() {
        let identity = vault()
            .generate_ssh_identity(SshKeyType::Ed25519, "")
            .unwrap();
        // OpenSSH fingerprint is over the wire-format public-key blob, not
        // over the textual form. Round-tripping through `ssh-key` lets us
        // reconstruct that blob without re-implementing the wire codec.
        let txt = std::str::from_utf8(&identity.public_key_openssh).unwrap();
        let pk: ssh_key::PublicKey = txt.parse().unwrap();
        let blob = pk.to_bytes().unwrap();
        let digest = sha256(&blob);
        let expected = format!(
            "SHA256:{}",
            BASE64_STANDARD.encode(digest).trim_end_matches('=')
        );
        assert_eq!(identity.fingerprint_sha256, expected);
    }

    #[test]
    fn round_trip_decrypt_recovers_openssh_pem() {
        let v = vault();
        let identity = v.generate_ssh_identity(SshKeyType::Ed25519, "").unwrap();
        let pem = v
            .decrypt_private_key(identity.encrypted_private_key.as_bytes())
            .unwrap();
        let pem_str = std::str::from_utf8(&pem).unwrap();
        assert!(pem_str.contains("-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(pem_str.contains("-----END OPENSSH PRIVATE KEY-----"));
    }

    #[test]
    fn ciphertext_differs_from_pem() {
        let v = vault();
        let identity = v.generate_ssh_identity(SshKeyType::Ed25519, "").unwrap();
        let ciphertext = identity.encrypted_private_key.as_bytes();
        let needle = b"-----BEGIN OPENSSH PRIVATE KEY-----";
        assert!(
            !ciphertext.windows(needle.len()).any(|w| w == needle),
            "ciphertext must not contain the plaintext PEM header",
        );
    }

    #[test]
    fn decrypt_with_wrong_master_key_fails() {
        let v = vault();
        let identity = v.generate_ssh_identity(SshKeyType::Ed25519, "").unwrap();
        let other = VaultService::new(VaultMasterKey::from_bytes([0x00u8; 32]));
        let err = other
            .decrypt_private_key(identity.encrypted_private_key.as_bytes())
            .unwrap_err();
        assert!(matches!(err, VaultError::Decrypt));
    }

    #[test]
    fn unsupported_key_types_are_rejected() {
        let err = vault()
            .generate_ssh_identity(SshKeyType::Rsa, "")
            .unwrap_err();
        assert!(matches!(err, VaultError::UnsupportedKeyType("rsa")));
    }

    #[test]
    fn comment_is_baked_into_public_key() {
        let identity = vault()
            .generate_ssh_identity(SshKeyType::Ed25519, "homelab-admin")
            .unwrap();
        let public = std::str::from_utf8(&identity.public_key_openssh).unwrap();
        assert!(
            public.ends_with(" homelab-admin"),
            "expected comment in public key, got: {public}",
        );
    }

    #[test]
    fn empty_comment_omits_trailing_text() {
        let identity = vault()
            .generate_ssh_identity(SshKeyType::Ed25519, "")
            .unwrap();
        let public = std::str::from_utf8(&identity.public_key_openssh).unwrap();
        let parts: Vec<&str> = public.split_whitespace().collect();
        assert_eq!(
            parts.len(),
            2,
            "no comment should leave exactly algo + base64, got: {public}",
        );
    }

    // -----------------------------------------------------------------
    // Import path tests.
    //
    // Throwaway test fixtures only — every PEM in this module is
    // generated for the test (or, for the encrypted fixture, generated
    // once with `ssh-keygen -t ed25519 -N test-fixture-passphrase`
    // against `/tmp` and immediately discarded). NEVER paste a personal
    // or production key here.
    // -----------------------------------------------------------------

    /// Generate a fresh Ed25519 OpenSSH PEM for a test. Returns
    /// `(pem_text, expected_fingerprint)` so the caller can assert the
    /// import round-trip without re-deriving the fingerprint.
    fn fresh_ed25519_pem(comment: &str) -> (Zeroizing<String>, String) {
        let mut private = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
        if !comment.is_empty() {
            private.set_comment(comment);
        }
        let fingerprint = private
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();
        let pem = private.to_openssh(LineEnding::LF).unwrap();
        (pem, fingerprint)
    }

    /// Encrypted Ed25519 OpenSSH PEM. Generated with
    /// `ssh-keygen -t ed25519 -N test-fixture-passphrase ...` against
    /// `/tmp`; the generator output and fixture file were both discarded
    /// after copying the PEM here. Throwaway material only — the
    /// passphrase is irrelevant because the test never decrypts the
    /// blob, only asserts the encrypted-format detection.
    const ENCRYPTED_ED25519_FIXTURE: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABDGGthwH6
5HvKYta7+oUveZAAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIK3NKUNWd0vLAj5U
LXdNXGZkqk325MEKbaoK099DkEnRAAAAsH8PDi1T/YYQiYvbBUIJ7w8MnJqaxmY/PIodsv
ViUddfryP2FpzZjwF1FYAkaKu5/wCuPlw1GsFEZ8PaD7B6Apvcqg1Zcrt+EtI1oYf4NhHj
nsizmmEEBm9fa/TmMjc6zd+lH7NzgG3cZ2va51bWxl+qafIoon1h42WANBO8MT3Y6DaqM8
k26TlD4VczWicURvSL8xj85+cwNCYIdBieUb3Ahyh0hKsZgI3f/88PcluO
-----END OPENSSH PRIVATE KEY-----
";

    /// Unencrypted RSA-2048 OpenSSH PEM. Generated with
    /// `ssh-keygen -t rsa -b 2048 -N '' ...` against `/tmp`; the
    /// generator output and fixture file were both discarded after
    /// copying the PEM here. Throwaway material only — used to pin the
    /// "vault rejects RSA at import" wire shape.
    const UNENCRYPTED_RSA_FIXTURE: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABFwAAAAdzc2gtcn
NhAAAAAwEAAQAAAQEAg/E3ABtl3xAKWJ5Qdekp4ly59yHr8vExhHynP7om4DUS4eNkzsNm
zDQI3KwKE+dJZZxLJZmgNPKw1Imj/EbwAEuvrBdShNf2jPlEKvaqnW1fjfxqAtguocISMh
uFKduxjODOigWnKtiYE54b+OAjBcUEdQg5+LeKRGQCwb3olzhyCWQKSQGdGDTbpOTHjrWD
CoTsBXcUWpCowkHzbdHajGgCbVwYJg5lfrvelG88JwNuogk+YL9rDMf5shQioqRv7WNAZ8
+Fn2SAsvdLWeFc5I43kKKiJnR1cZBLZOinrgi1BYNJbloA1uiJHvJd+Cpts75Au8wlRRHV
hUqKqNX7EwAAA+B8rkMWfK5DFgAAAAdzc2gtcnNhAAABAQCD8TcAG2XfEApYnlB16SniXL
n3Ievy8TGEfKc/uibgNRLh42TOw2bMNAjcrAoT50llnEslmaA08rDUiaP8RvAAS6+sF1KE
1/aM+UQq9qqdbV+N/GoC2C6hwhIyG4Up27GM4M6KBacq2JgTnhv44CMFxQR1CDn4t4pEZA
LBveiXOHIJZApJAZ0YNNuk5MeOtYMKhOwFdxRakKjCQfNt0dqMaAJtXBgmDmV+u96Ubzwn
A26iCT5gv2sMx/myFCKipG/tY0Bnz4WfZICy90tZ4VzkjjeQoqImdHVxkEtk6KeuCLUFg0
luWgDW6Ike8l34Km2zvkC7zCVFEdWFSoqo1fsTAAAAAwEAAQAAAQAA/dSQeyQ6V2gEf3gS
UsS+Tz0Uhtw7kKVzHe6x02fMYom4SdmtlhlVKoTwh5hxytip21FTQILMMxCyIDCryiquje
MNk4VKu0a+i3cALaddlH9V1VJEoDRFgexaFQvcoyqD6QKUVfOKJmOKLjN+nMyWlALzEDND
U7nFxsyggRlY3ZB2qfDZm+IMXvOpg957Ymx9EKk6btlK5S7L9Q8tSNBH9r/LFsPRuntGSo
J/h6Cj5Ey8RzmTqGsppX+eEFDC9y/6qiTJxcs+w2fQ84PLjyxdV3NgF+ptMI58R5pdXyw2
v0Awd9KRLZtTQrD5siUlWFTwiBudiv9W4YfkuoxUMv61AAAAgEh20bdfG5ek+didpVF+LF
PJr874Z9gHaWf4CW6vxsxn7d+6M79Qj8m5V7PrcpfatUVO+UH+acOaYTDAznJAKL973CUw
+pf2tE26/8HZwE1eg1MYbZxh/K6AX0Uom0ScUNOl+TEOcebFzlP3yVRvwkGiY6Cat9247+
wMSJB5UIvJAAAAgQC49W/XTxmISC3l73yw+p1aOBehQj0hFon4mPJMtMkfoK1ccyjgBc22
uTINkwLdWU3aRb+qbDcAhjSujuAFpYJC1UYAl8BZlJ3Cz9V8hgPWWRFrSF0j7AE4zi69PY
qH8JqMyn+dBbiDSCL+AQaO8CMVWGbdOTnB4olWRP6BpnksrQAAAIEAtp7KFp8Us70N9bnP
Am883wQ/6s+pVFdFe9lWVsFbFXstY8mRnDC61VrVCWIKTNxLeYzothAVol66JB7R9mCzfP
W+aS4ZjwieaojGdJq0PcP7QXk9r43i5vTxdsaqfVhZI3BJkWEA3xawoQHKGqq8mzjADSU1
qmZ/xxuugbvC/r8AAAAncmVsYXl0ZXJtLXRlc3QtZml4dHVyZS1kby1ub3QtdHJ1c3Qtcn
NhAQIDBA==
-----END OPENSSH PRIVATE KEY-----
";

    #[test]
    fn import_round_trips_ed25519_fingerprint() {
        let v = vault();
        let (pem, expected_fp) = fresh_ed25519_pem("homelab-admin");
        let identity = v
            .import_ssh_identity(pem.as_bytes(), "imported-name")
            .unwrap();
        assert_eq!(identity.key_type, SshKeyType::Ed25519);
        assert_eq!(identity.fingerprint_sha256, expected_fp);
        let public = std::str::from_utf8(&identity.public_key_openssh).unwrap();
        assert!(public.starts_with("ssh-ed25519 "));
        // The supplied name overrides the original comment baked into the
        // PEM — operators expect "the name in RelayTerm matches the
        // authorized_keys comment on the target host."
        assert!(
            public.ends_with(" imported-name"),
            "imported public key should bake the supplied name as the OpenSSH comment, got: {public}",
        );
    }

    #[test]
    fn import_returns_ciphertext_not_plaintext() {
        let v = vault();
        let (pem, _) = fresh_ed25519_pem("");
        let identity = v.import_ssh_identity(pem.as_bytes(), "").unwrap();
        let ciphertext = identity.encrypted_private_key.as_bytes();
        let needle = b"BEGIN OPENSSH PRIVATE KEY";
        assert!(
            !ciphertext.windows(needle.len()).any(|w| w == needle),
            "imported ciphertext must not contain the plaintext PEM header",
        );
        // Envelope magic should still front the blob.
        assert_eq!(&ciphertext[..4], b"RTV1");
    }

    #[test]
    fn import_round_trip_decrypt_recovers_canonical_pem() {
        let v = vault();
        let (pem, _) = fresh_ed25519_pem("");
        let identity = v.import_ssh_identity(pem.as_bytes(), "round-trip").unwrap();
        let recovered = v
            .decrypt_private_key(identity.encrypted_private_key.as_bytes())
            .unwrap();
        let recovered_str = std::str::from_utf8(&recovered).unwrap();
        assert!(recovered_str.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
        // The decrypt path returns the canonical re-serialized form, so the
        // round-trip exercises every byte of the import pipeline.
        let parsed = ssh_key::PrivateKey::from_openssh(recovered_str).unwrap();
        assert_eq!(parsed.algorithm(), Algorithm::Ed25519);
    }

    #[test]
    fn import_rejects_garbage_with_malformed() {
        let v = vault();
        let err = v
            .import_ssh_identity(b"not a key", "name")
            .expect_err("must reject garbage");
        assert!(matches!(
            err,
            VaultError::UnsupportedFormat {
                reason: "malformed"
            }
        ));
    }

    #[test]
    fn import_rejects_non_ascii_with_malformed() {
        let v = vault();
        let err = v
            .import_ssh_identity(b"\xff\xfe garbage", "name")
            .expect_err("must reject non-ASCII");
        assert!(matches!(
            err,
            VaultError::UnsupportedFormat {
                reason: "malformed"
            }
        ));
    }

    #[test]
    fn import_rejects_encrypted_with_typed_reason() {
        let v = vault();
        let err = v
            .import_ssh_identity(ENCRYPTED_ED25519_FIXTURE.as_bytes(), "name")
            .expect_err("encrypted PEM must be rejected");
        assert!(matches!(
            err,
            VaultError::UnsupportedFormat {
                reason: "encrypted"
            }
        ));
    }

    #[test]
    fn import_rejects_rsa_with_typed_key_type() {
        let v = vault();
        let err = v
            .import_ssh_identity(UNENCRYPTED_RSA_FIXTURE.as_bytes(), "name")
            .expect_err("RSA must be rejected by the import surface");
        assert!(matches!(err, VaultError::UnsupportedKeyType("rsa")));
    }

    #[test]
    fn import_does_not_leak_pem_in_returned_struct_debug() {
        let v = vault();
        let (pem, _) = fresh_ed25519_pem("");
        let identity = v.import_ssh_identity(pem.as_bytes(), "name").unwrap();
        let debug = format!("{identity:?}");
        // The redacted-Debug discipline on `GeneratedSshIdentity` and
        // `EncryptedBlob` keeps the PEM marker out of any formatter output.
        assert!(!debug.contains("BEGIN OPENSSH PRIVATE KEY"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn debug_does_not_leak_master_key_or_ciphertext() {
        let v = vault();
        let identity = v.generate_ssh_identity(SshKeyType::Ed25519, "").unwrap();

        let svc = format!("{v:?}");
        assert!(
            svc.contains("redacted"),
            "service debug should redact key: {svc}"
        );

        let id = format!("{identity:?}");
        assert!(
            id.contains("redacted ciphertext"),
            "identity debug should redact ciphertext: {id}"
        );
        // Public key bytes are not redacted (they're public), but plaintext
        // PEM markers must never appear — every reachable byte is ciphertext.
        assert!(!id.contains("BEGIN OPENSSH PRIVATE KEY"));
    }
}
