use k256::ecdsa::signature::hazmat::PrehashVerifier;
use k256::ecdsa::{SigningKey, VerifyingKey};
use k256::elliptic_curve::Curve;
use k256::elliptic_curve::ops::Reduce;
use k256::{NonZeroScalar, Scalar, U256};
use sha2::{Digest, Sha512};

const ZERO_U256: U256 = U256::ZERO;

use rxrpl_primitives::{PublicKey, Signature};

use crate::der;
use crate::seed::Seed;
use crate::sha512_half::sha512_half_single;

/// Derive a scalar from bytes using the XRPL iterative SHA-512/2 method.
///
/// This hashes `bytes || discriminator(4 LE bytes) || counter(4 LE bytes)`,
/// takes the first 32 bytes, and checks if it's a valid non-zero scalar
/// less than the curve order.
fn derive_scalar(bytes: &[u8], discriminator: Option<u32>) -> Scalar {
    let order = k256::Secp256k1::ORDER;

    for i in 0u32.. {
        let mut hasher = Sha512::new();
        hasher.update(bytes);

        if let Some(discrim) = discriminator {
            hasher.update(discrim.to_le_bytes());
        }

        hasher.update(i.to_le_bytes());

        let hash = hasher.finalize();
        let candidate = U256::from_be_slice(&hash[..32]);

        // Only accept if candidate is non-zero and less than the curve order
        if candidate == ZERO_U256 || candidate >= order {
            continue;
        }

        let scalar = <Scalar as Reduce<U256>>::reduce(candidate);
        return scalar;
    }
    unreachable!("impossible: no valid scalar found after 2^32 iterations")
}

/// Derive a secp256k1 keypair from a seed.
///
/// For regular (account) keys:
///   1. Derive root private generator from seed
///   2. Compute root public key
///   3. Derive additional scalar from root public key with discriminator=0
///   4. private_key = (root_gen + additional_scalar) mod order
///
/// For validator keys, only the root generator is used.
pub fn derive_keypair(seed: &Seed, validator: bool) -> (PublicKey, Vec<u8>) {
    let private_gen = derive_scalar(seed.as_bytes(), None);

    let private_key = if validator {
        private_gen
    } else {
        let nz = NonZeroScalar::new(private_gen).unwrap();
        let root_signing = SigningKey::from(nz);
        let root_pubkey = root_signing.verifying_key();
        let root_pubkey_bytes = root_pubkey.to_encoded_point(true);

        let derived_scalar = derive_scalar(root_pubkey_bytes.as_bytes(), Some(0));
        private_gen + derived_scalar
    };

    let nz = NonZeroScalar::new(private_key).unwrap();
    let signing_key = SigningKey::from(nz);
    let verifying_key = signing_key.verifying_key();
    let pub_bytes = verifying_key.to_encoded_point(true).as_bytes().to_vec();
    let pub_key = PublicKey(pub_bytes);

    // Private key with 0x00 prefix (XRPL convention)
    let mut priv_bytes = vec![0x00];
    priv_bytes.extend_from_slice(&private_key.to_bytes());

    (pub_key, priv_bytes)
}

/// Sign a pre-hashed 32-byte digest directly with a secp256k1 private key.
///
/// Unlike `sign()`, this does NOT hash the input -- it signs the digest as-is.
/// Used for protocols that provide the digest directly (e.g., rippled session cookies).
/// Returns a DER-encoded signature.
pub fn sign_digest(digest: &[u8; 32], private_key: &[u8]) -> Result<Signature, crate::CryptoError> {
    let key_bytes = if private_key.len() == 33 && private_key[0] == 0x00 {
        &private_key[1..]
    } else if private_key.len() == 32 {
        private_key
    } else {
        return Err(crate::CryptoError::InvalidPrivateKey);
    };

    let signing_key =
        SigningKey::from_slice(key_bytes).map_err(|_| crate::CryptoError::InvalidPrivateKey)?;

    let (sig, _) = signing_key
        .sign_prehash_recoverable(digest)
        .map_err(|_| crate::CryptoError::SigningFailed)?;

    let normalized = sig.normalize_s().unwrap_or(sig);
    let (r_bytes, s_bytes) = normalized.split_bytes();

    let der_sig = der::encode_der_signature(&r_bytes, &s_bytes);
    Ok(Signature::new(der_sig))
}

/// Sign a message (raw bytes) with a secp256k1 private key.
///
/// The message is first hashed with SHA-512/2, then signed with ECDSA.
/// Returns a DER-encoded signature.
pub fn sign(message: &[u8], private_key: &[u8]) -> Result<Signature, crate::CryptoError> {
    // Strip 0x00 prefix if present
    let key_bytes = if private_key.len() == 33 && private_key[0] == 0x00 {
        &private_key[1..]
    } else if private_key.len() == 32 {
        private_key
    } else {
        return Err(crate::CryptoError::InvalidPrivateKey);
    };

    let signing_key =
        SigningKey::from_slice(key_bytes).map_err(|_| crate::CryptoError::InvalidPrivateKey)?;

    let hash = sha512_half_single(message);
    let (sig, _) = signing_key
        .sign_prehash_recoverable(hash.as_bytes())
        .map_err(|_| crate::CryptoError::SigningFailed)?;

    // Normalize S to low-S form for full canonicality
    let normalized = sig.normalize_s().unwrap_or(sig);
    let (r_bytes, s_bytes) = normalized.split_bytes();

    let der_sig = der::encode_der_signature(&r_bytes, &s_bytes);
    Ok(Signature::new(der_sig))
}

/// Verify a DER-encoded secp256k1 signature against a message and public key.
pub fn verify(message: &[u8], public_key: &[u8], signature: &[u8]) -> bool {
    let Ok(verifying_key) = VerifyingKey::from_sec1_bytes(public_key) else {
        return false;
    };

    let Ok((r_bytes, s_bytes)) = der::decode_der_signature(signature) else {
        return false;
    };

    // Reconstruct k256 signature from r,s
    let mut r_padded = [0u8; 32];
    let mut s_padded = [0u8; 32];
    let r_start = 32usize.saturating_sub(r_bytes.len());
    let s_start = 32usize.saturating_sub(s_bytes.len());
    r_padded[r_start..].copy_from_slice(&r_bytes);
    s_padded[s_start..].copy_from_slice(&s_bytes);

    let Ok(sig) = k256::ecdsa::Signature::from_scalars(r_padded, s_padded) else {
        return false;
    };

    let hash = sha512_half_single(message);
    verifying_key.verify_prehash(hash.as_bytes(), &sig).is_ok()
}

/// Verify a DER-encoded secp256k1 signature against a pre-hashed 32-byte digest.
///
/// Unlike `verify()`, this does NOT hash the input.
pub fn verify_digest(digest: &[u8; 32], public_key: &[u8], signature: &[u8]) -> bool {
    let Ok(verifying_key) = VerifyingKey::from_sec1_bytes(public_key) else {
        return false;
    };

    let Ok((r_bytes, s_bytes)) = der::decode_der_signature(signature) else {
        return false;
    };

    let mut r_padded = [0u8; 32];
    let mut s_padded = [0u8; 32];
    let r_start = 32usize.saturating_sub(r_bytes.len());
    let s_start = 32usize.saturating_sub(s_bytes.len());
    r_padded[r_start..].copy_from_slice(&r_bytes);
    s_padded[s_start..].copy_from_slice(&s_bytes);

    let Ok(sig) = k256::ecdsa::Signature::from_scalars(r_padded, s_padded) else {
        return false;
    };

    verifying_key.verify_prehash(digest, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_keypair_test_vector_1() {
        // From goXRPLd test: seed bytes -> expected pubkey/privkey
        let seed = Seed::from_bytes([
            229, 81, 182, 134, 131, 220, 192, 126, 133, 114, 150, 132, 140, 237, 222, 196,
        ]);
        let (pub_key, priv_key) = derive_keypair(&seed, false);

        assert_eq!(
            hex::encode_upper(pub_key.as_bytes()),
            "02950F4710101A25073BF37086D73FBBD00C7A6B0F91097D8F0BC6D268C400D56E"
        );
        assert_eq!(
            hex::encode_upper(&priv_key),
            "00B167A9F3B9E60A4F93695713682C102438620AA1785C3AE635F53E5B6261071A"
        );
    }

    #[test]
    fn derive_keypair_test_vector_2() {
        let seed = Seed::from_bytes([
            124, 228, 51, 247, 54, 54, 81, 51, 239, 86, 226, 187, 232, 20, 111, 163,
        ]);
        let (pub_key, priv_key) = derive_keypair(&seed, false);

        assert_eq!(
            hex::encode_upper(pub_key.as_bytes()),
            "031FBCFDD2EC6C2EDFBBA3866BDBAC28E5253C6A01FE9EFF8CAAE01871F009E837"
        );
        assert_eq!(
            hex::encode_upper(&priv_key),
            "00A3D1513DBE784107428B363A1F8EAF1377AB63D4D137AB9E28E0BC614C71D8C0"
        );
    }

    #[test]
    fn sign_and_verify() {
        let seed = Seed::from_bytes([
            229, 81, 182, 134, 131, 220, 192, 126, 133, 114, 150, 132, 140, 237, 222, 196,
        ]);
        let (pub_key, priv_key) = derive_keypair(&seed, false);

        let message = b"test message";
        let sig = sign(message, &priv_key).unwrap();
        assert!(verify(message, pub_key.as_bytes(), sig.as_bytes()));

        // Tampered message should fail
        assert!(!verify(
            b"wrong message",
            pub_key.as_bytes(),
            sig.as_bytes()
        ));
    }
}
