use ed25519_dalek::{Signer, Verifier};

use rxrpl_primitives::{PublicKey, Signature};

use crate::seed::Seed;
use crate::sha512_half::sha512_half_single;

/// Derive an Ed25519 keypair from a seed.
///
/// 1. SHA-512/2 the seed to get 32 bytes
/// 2. Use those 32 bytes as the Ed25519 secret key seed
/// 3. Derive the public key
///
/// Returns (public_key with 0xED prefix, private_key with 0xED prefix).
pub fn derive_keypair(seed: &Seed) -> (PublicKey, Vec<u8>) {
    let raw_priv = sha512_half_single(seed.as_bytes());

    let signing_key = ed25519_dalek::SigningKey::from_bytes(raw_priv.as_bytes());
    let verifying_key = signing_key.verifying_key();

    // Public key: 0xED prefix + 32-byte key
    let mut pub_bytes = Vec::with_capacity(33);
    pub_bytes.push(0xED);
    pub_bytes.extend_from_slice(verifying_key.as_bytes());
    let pub_key = PublicKey(pub_bytes);

    // Private key: 0xED prefix + 32-byte seed (NOT the full 64-byte key)
    let mut priv_bytes = Vec::with_capacity(33);
    priv_bytes.push(0xED);
    priv_bytes.extend_from_slice(raw_priv.as_bytes());

    (pub_key, priv_bytes)
}

/// Sign a message (raw bytes) with an Ed25519 private key.
///
/// The private key should be 33 bytes (0xED + 32-byte seed).
/// Returns a 64-byte Ed25519 signature.
pub fn sign(message: &[u8], private_key: &[u8]) -> Result<Signature, crate::CryptoError> {
    let key_bytes = if private_key.len() == 33 && private_key[0] == 0xED {
        &private_key[1..]
    } else if private_key.len() == 32 {
        private_key
    } else {
        return Err(crate::CryptoError::InvalidPrivateKey);
    };

    let signing_key = ed25519_dalek::SigningKey::from_bytes(
        key_bytes
            .try_into()
            .map_err(|_| crate::CryptoError::InvalidPrivateKey)?,
    );

    let sig = signing_key.sign(message);
    Ok(Signature::new(sig.to_bytes().to_vec()))
}

/// Verify an Ed25519 signature against a message and public key.
///
/// The public key should be 33 bytes (0xED + 32-byte key).
pub fn verify(message: &[u8], public_key: &[u8], signature: &[u8]) -> bool {
    let key_bytes = if public_key.len() == 33 && public_key[0] == 0xED {
        &public_key[1..]
    } else if public_key.len() == 32 {
        public_key
    } else {
        return false;
    };

    let Ok(key_arr): Result<[u8; 32], _> = key_bytes.try_into() else {
        return false;
    };
    let Ok(verifying_key) = ed25519_dalek::VerifyingKey::from_bytes(&key_arr) else {
        return false;
    };

    let Ok(sig_arr): Result<[u8; 64], _> = signature.try_into() else {
        return false;
    };
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);

    verifying_key.verify(message, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_keypair_test_vector() {
        // From goXRPLd: seed "fakeRandomString" bytes
        let seed = Seed::from_bytes([
            102, 97, 107, 101, 82, 97, 110, 100, 111, 109, 83, 116, 114, 105, 110, 103,
        ]);
        let (pub_key, priv_key) = derive_keypair(&seed);

        assert_eq!(
            hex::encode_upper(pub_key.as_bytes()),
            "ED4924A9045FE5ED8B22BAA7B6229A72A287CCF3EA287AADD3A032A24C0F008FA6"
        );
        assert_eq!(
            hex::encode_upper(&priv_key),
            "EDBB3ECA8985E1484FA6A28C4B30FB0042A2CC5DF3EC8DC37B5F3D126DDFD3CA14"
        );
    }

    #[test]
    fn sign_test_vector() {
        // From goXRPLd: signing "hello world" with known key
        let priv_key =
            hex::decode("EDBB3ECA8985E1484FA6A28C4B30FB0042A2CC5DF3EC8DC37B5F3D126DDFD3CA14")
                .unwrap();
        let sig = sign(b"hello world", &priv_key).unwrap();
        assert_eq!(
            hex::encode_upper(sig.as_bytes()),
            "E83CAFEAF100793F0C6570D60C7447FF3A87E0DC0CAE9AD90EF0102860EC3BD1D20F432494021F3E19DAFF257A420CA64A49C283AB5AD00B6B0CEA1756151C01"
        );
    }

    #[test]
    fn verify_test_vector() {
        let pub_key =
            hex::decode("ED4924A9045FE5ED8B22BAA7B6229A72A287CCF3EA287AADD3A032A24C0F008FA6")
                .unwrap();
        let sig = hex::decode("C001CB8A9883497518917DD16391930F4FEE39CEA76C846CFF4330BA44ED19DC4730056C2C6D7452873DE8120A5023C6807135C6329A89A13BA1D476FE8E7100").unwrap();
        assert!(verify(b"test message", &pub_key, &sig));
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let seed = Seed::random();
        let (pub_key, priv_key) = derive_keypair(&seed);

        let message = b"hello world";
        let sig = sign(message, &priv_key).unwrap();
        assert!(verify(message, pub_key.as_bytes(), sig.as_bytes()));
        assert!(!verify(b"wrong", pub_key.as_bytes(), sig.as_bytes()));
    }
}
