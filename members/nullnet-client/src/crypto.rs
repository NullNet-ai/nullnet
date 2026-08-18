use aes_gcm::aead::{Aead, Generate, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};

/// AES-256-GCM cipher for one VLAN tunnel's traffic. Wire format produced by
/// [`Self::encrypt`] / consumed by [`Self::decrypt`] is `nonce[12] || ciphertext+tag`.
/// Unlike `nullnet-server`'s cert-at-rest `Encryptor` (keyed once, process-wide,
/// operates on UTF-8 strings), this is constructed per-tunnel from the key the
/// server hands out at `VlanSetup` time and operates on raw Ethernet frames.
pub(crate) struct TunnelCipher {
    cipher: Aes256Gcm,
}

impl TunnelCipher {
    pub(crate) fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: Aes256Gcm::new(<&Key<Aes256Gcm>>::from(key)),
        }
    }

    pub(crate) fn encrypt(&self, plaintext: &[u8]) -> Option<Vec<u8>> {
        let nonce_bytes: [u8; 12] = Generate::generate();
        let mut out = nonce_bytes.to_vec();
        out.extend(
            self.cipher
                .encrypt(&Nonce::from(nonce_bytes), plaintext)
                .ok()?,
        );
        Some(out)
    }

    pub(crate) fn decrypt(&self, data: &[u8]) -> Option<Vec<u8>> {
        if data.len() < 12 {
            return None;
        }
        let (nonce_bytes, ciphertext) = data.split_at(12);
        self.cipher
            .decrypt(&Nonce::try_from(nonce_bytes).ok()?, ciphertext)
            .ok()
    }
}

/// Wire framing for the VLAN userspace forwarder (`forward/send.rs`,
/// `forward/receive.rs`): every datagram on the forward socket is
/// `vlan_id[2, big-endian] || nonce[12] || ciphertext+tag`. The vlan_id has
/// to be readable in the clear so the receiver knows which tunnel's key to
/// decrypt with before it can read anything else.
pub(crate) fn seal(vlan_id: u16, cipher: &TunnelCipher, plaintext: &[u8]) -> Option<Vec<u8>> {
    let mut out = vlan_id.to_be_bytes().to_vec();
    out.extend(cipher.encrypt(plaintext)?);
    Some(out)
}

/// Wire framing for a VLAN tunnel with encryption disabled: the same
/// `vlan_id[2, big-endian] || ...` prefix as `seal`, but the frame follows
/// as-is afterward — no nonce, no AEAD transform. Both ends agree on which
/// framing applies to a given `vlan_id` from the same `VlanSetup.encrypted`
/// flag, so there's no ambiguity despite the shorter format.
pub(crate) fn seal_plain(vlan_id: u16, plaintext: &[u8]) -> Vec<u8> {
    let mut out = vlan_id.to_be_bytes().to_vec();
    out.extend_from_slice(plaintext);
    out
}

/// Splits a raw forward-socket datagram into its cleartext `vlan_id` and the
/// remaining `nonce || ciphertext+tag` slice, ready for `TunnelCipher::decrypt`.
pub(crate) fn open_vlan_id(datagram: &[u8]) -> Option<(u16, &[u8])> {
    if datagram.len() < 2 {
        return None;
    }
    let (vlan_id_bytes, rest) = datagram.split_at(2);
    Some((
        u16::from_be_bytes([vlan_id_bytes[0], vlan_id_bytes[1]]),
        rest,
    ))
}

#[cfg(test)]
mod tests {
    use super::{TunnelCipher, open_vlan_id, seal, seal_plain};

    #[test]
    fn round_trip() {
        let cipher = TunnelCipher::new(&[7u8; 32]);
        let frame = b"pretend this is an ethernet frame";
        let ct = cipher.encrypt(frame).unwrap();
        assert_ne!(ct, frame);
        assert_eq!(cipher.decrypt(&ct).unwrap(), frame);
    }

    #[test]
    fn nonce_randomizes_ciphertext() {
        let cipher = TunnelCipher::new(&[7u8; 32]);
        assert_ne!(
            cipher.encrypt(b"same").unwrap(),
            cipher.encrypt(b"same").unwrap()
        );
    }

    #[test]
    fn wrong_key_fails() {
        let ct = TunnelCipher::new(&[1u8; 32]).encrypt(b"secret").unwrap();
        assert!(TunnelCipher::new(&[2u8; 32]).decrypt(&ct).is_none());
    }

    #[test]
    fn truncated_data_fails_without_panicking() {
        let cipher = TunnelCipher::new(&[3u8; 32]);
        assert!(cipher.decrypt(&[0u8; 5]).is_none());
    }

    #[test]
    fn seal_then_open_round_trips_vlan_id_and_plaintext() {
        let cipher = TunnelCipher::new(&[9u8; 32]);
        let frame = b"ethernet frame payload";
        let datagram = seal(4242, &cipher, frame).unwrap();

        let (vlan_id, rest) = open_vlan_id(&datagram).unwrap();
        assert_eq!(vlan_id, 4242);
        assert_eq!(cipher.decrypt(rest).unwrap(), frame);
    }

    #[test]
    fn seal_plain_then_open_round_trips_vlan_id_and_frame_untransformed() {
        let frame = b"ethernet frame payload";
        let datagram = seal_plain(4242, frame);

        let (vlan_id, rest) = open_vlan_id(&datagram).unwrap();
        assert_eq!(vlan_id, 4242);
        assert_eq!(rest, frame);
    }

    #[test]
    fn open_vlan_id_rejects_short_datagrams() {
        assert!(open_vlan_id(&[0u8]).is_none());
        assert!(open_vlan_id(&[]).is_none());
    }
}
