//! Authenticated encryption for peer credentials that Conduit must present.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::rand::{SecureRandom, SystemRandom};

const NONCE_BYTES: usize = 12;

#[derive(Clone)]
pub(crate) struct LinkTokenCipher(LessSafeKey);

impl LinkTokenCipher {
    pub(crate) fn new(key: &[u8]) -> Result<Self, String> {
        let key = UnboundKey::new(&AES_256_GCM, key)
            .map_err(|_| "link token encryption key must be 32 bytes".to_owned())?;
        Ok(Self(LessSafeKey::new(key)))
    }

    pub(crate) fn encrypt(&self, peer_id: &str, plaintext: &str) -> Result<String, String> {
        let mut nonce_bytes = [0; NONCE_BYTES];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| "could not generate a link token nonce".to_owned())?;
        let mut in_out = plaintext.as_bytes().to_vec();
        self.0
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(peer_id.as_bytes()),
                &mut in_out,
            )
            .map_err(|_| "could not encrypt a link token".to_owned())?;
        let mut sealed = nonce_bytes.to_vec();
        sealed.extend_from_slice(&in_out);
        Ok(URL_SAFE_NO_PAD.encode(sealed))
    }

    pub(crate) fn decrypt(&self, peer_id: &str, ciphertext: &str) -> Result<String, String> {
        let mut sealed = URL_SAFE_NO_PAD
            .decode(ciphertext)
            .map_err(|_| "stored peer token ciphertext is invalid".to_owned())?;
        if sealed.len() <= NONCE_BYTES {
            return Err("stored peer token ciphertext is too short".to_owned());
        }
        let nonce_bytes: [u8; NONCE_BYTES] =
            sealed[..NONCE_BYTES].try_into().expect("fixed nonce length");
        let plaintext = self
            .0
            .open_in_place(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(peer_id.as_bytes()),
                &mut sealed[NONCE_BYTES..],
            )
            .map_err(|_| "stored peer token ciphertext cannot be authenticated".to_owned())?;
        String::from_utf8(plaintext.to_vec())
            .map_err(|_| "stored peer token is not UTF-8".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_token_ciphertext_round_trips_without_revealing_plaintext() {
        let cipher = LinkTokenCipher::new(&[7; 32]).unwrap();
        let ciphertext = cipher.encrypt("dicta-office", "peer-token-value").unwrap();
        assert!(!ciphertext.contains("peer-token-value"));
        assert_eq!(cipher.decrypt("dicta-office", &ciphertext).unwrap(), "peer-token-value");
    }

    #[test]
    fn peer_token_ciphertext_rejects_tampering_and_wrong_keys() {
        let ciphertext =
            LinkTokenCipher::new(&[7; 32]).unwrap().encrypt("dicta-office", "secret").unwrap();
        let wrong = LinkTokenCipher::new(&[8; 32]).unwrap();
        assert!(wrong.decrypt("dicta-office", &ciphertext).is_err());
        let same_key = LinkTokenCipher::new(&[7; 32]).unwrap();
        assert!(same_key.decrypt("dicta-other", &ciphertext).is_err());
    }
}
