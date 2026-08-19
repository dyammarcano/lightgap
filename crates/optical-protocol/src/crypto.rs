//! Pairing and per-PDU encryption.
//!
//! The optical channel has an unusual security property worth exploiting: it is
//! line-of-sight and short range. Intercepting it means physically placing a
//! camera between two displays that are pointed at each other, which is
//! considerably harder than joining a wireless network. That does not make it
//! safe, but it does mean the realistic attack is a man-in-the-middle who
//! substitutes their own key during pairing, not a passive eavesdropper.
//!
//! So the design is:
//!
//! - **X25519** over the visual channel to agree a secret.
//! - **A short authentication string** shown on both displays for the user to
//!   compare. This is what actually closes the man-in-the-middle hole; the key
//!   exchange alone does not, and a design that stops at the key exchange is
//!   security theatre.
//! - **ChaCha20-Poly1305 per PDU**, with separate keys per direction so the
//!   nonce can be the sequence number alone.
//!
//! **Why the nonce is derived rather than transmitted.** Twelve bytes per PDU on
//! a channel delivering a few hundred bytes per frame is between 1% and 6% of
//! the payload. Both sides already know the direction and the sequence number,
//! so sending it would be paying for information the receiver has.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use x25519_dalek::{PublicKey, ReusableSecret};

use crate::session::PeerId;

/// Context string for key derivation.
///
/// Domain separation: derived material from this protocol must never coincide
/// with material derived by anything else that happens to use the same shared
/// secret.
///
/// These two deliberately still say `qr_comm` after the project was renamed to
/// Lightgap. They are protocol constants, not branding. Every session key and
/// every authentication string is derived through them, so changing the text
/// changes the keys, and two builds that disagree on it cannot talk to each
/// other at all — the failure looks like corruption, not like a rename. What
/// they have to be is stable and unique, never on-brand. If they ever do
/// change, bump `v1` in the same edit so the break is deliberate and visible.
const KDF_CONTEXT: &str = "qr_comm v1 session key";
/// Context string for the authentication string.
const SAS_CONTEXT: &str = "qr_comm v1 short authentication string";

/// Which way a PDU is travelling, for key selection.
///
/// Named apart from [`crate::channel::Direction`] on purpose: that one describes
/// whether a medium can transmit, receive or both, whereas this one says which
/// peer is sending. Two same-named types meaning different things in one crate
/// is a trap worth avoiding.
///
/// It is part of the key derivation rather than the nonce, so each direction
/// gets its own key and the nonce can be the sequence number alone. With a
/// shared key the two directions would have to coordinate sequence numbers to
/// avoid nonce reuse, which is exactly the kind of coupling that eventually gets
/// it wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDirection {
    LeaderToFollower,
    FollowerToLeader,
}

impl KeyDirection {
    const fn label(self) -> &'static str {
        match self {
            Self::LeaderToFollower => "l2f",
            Self::FollowerToLeader => "f2l",
        }
    }

    /// The direction as seen from the other end.
    #[must_use]
    pub const fn flip(self) -> Self {
        match self {
            Self::LeaderToFollower => Self::FollowerToLeader,
            Self::FollowerToLeader => Self::LeaderToFollower,
        }
    }
}

/// Why a cryptographic operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("the peer's public key has not been received yet")]
    NoPeerKey,

    #[error("decryption failed: wrong key, or the payload was tampered with")]
    Decrypt,

    #[error("the peer's public key is malformed")]
    BadPeerKey,
}

/// One end's ephemeral identity for a session.
///
/// Ephemeral rather than long-lived on purpose. There is no identity to persist
/// here — the user is holding both devices — and a fresh key per session means a
/// compromise of one session cannot retroactively open another.
pub struct Identity {
    secret: ReusableSecret,
    public: PublicKey,
    /// Random material mixed into the derivation, so the session key differs
    /// even if the same pair of ephemeral keys somehow recurred.
    nonce: [u8; 16],
}

impl Identity {
    /// Generates a fresh identity from the operating system's entropy.
    #[must_use]
    pub fn generate() -> Self {
        let secret = ReusableSecret::random();
        let public = PublicKey::from(&secret);
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).expect("the OS must provide entropy");
        Self {
            secret,
            public,
            nonce,
        }
    }

    /// The public key to put in a `Hello`.
    #[must_use]
    pub fn public_bytes(&self) -> [u8; 32] {
        self.public.to_bytes()
    }

    #[must_use]
    pub fn nonce(&self) -> [u8; 16] {
        self.nonce
    }
}

/// Everything both sides agreed on, once pairing completes.
pub struct SessionKeys {
    leader_to_follower: ChaCha20Poly1305,
    follower_to_leader: ChaCha20Poly1305,
    /// The digits the user compares across the two displays.
    sas: String,
}

/// Number of digits in the authentication string.
///
/// Six decimal digits is a million possibilities, so a man-in-the-middle has a
/// one-in-a-million chance of producing a matching string. Short enough to read
/// aloud and compare without anyone giving up halfway, which matters more than
/// it sounds: an authentication string nobody actually checks provides no
/// security at all.
pub const SAS_DIGITS: usize = 6;

impl SessionKeys {
    /// Completes the key agreement.
    ///
    /// The two nonces are ordered by the peer identifiers rather than by who is
    /// calling, so both sides mix them in the same order and derive the same
    /// keys. Getting this wrong produces a session where each side can encrypt
    /// but neither can decrypt, which looks like a transport failure.
    pub fn agree(
        local_id: PeerId,
        local: &Identity,
        peer_id: PeerId,
        peer_public: &[u8; 32],
        peer_nonce: &[u8; 16],
    ) -> Result<Self, CryptoError> {
        let peer_key = PublicKey::from(*peer_public);
        let shared = local.secret.diffie_hellman(&peer_key);

        // All-zero shared secrets come from low-order public keys and mean the
        // peer is either broken or hostile.
        if shared.as_bytes().iter().all(|b| *b == 0) {
            return Err(CryptoError::BadPeerKey);
        }

        let (lo_nonce, hi_nonce) = if local_id <= peer_id {
            (local.nonce, *peer_nonce)
        } else {
            (*peer_nonce, local.nonce)
        };

        let mut material = Vec::with_capacity(32 + 32);
        material.extend_from_slice(shared.as_bytes());
        material.extend_from_slice(&lo_nonce);
        material.extend_from_slice(&hi_nonce);

        let l2f = blake3::derive_key(
            &format!("{KDF_CONTEXT} {}", KeyDirection::LeaderToFollower.label()),
            &material,
        );
        let f2l = blake3::derive_key(
            &format!("{KDF_CONTEXT} {}", KeyDirection::FollowerToLeader.label()),
            &material,
        );

        // The authentication string commits to both public keys as well as the
        // shared secret. Deriving it from the secret alone would let an attacker
        // who managed to force a shared secret produce a matching string.
        let (lo_pub, hi_pub) = if local_id <= peer_id {
            (local.public.to_bytes(), *peer_public)
        } else {
            (*peer_public, local.public.to_bytes())
        };
        let mut sas_material = material.clone();
        sas_material.extend_from_slice(&lo_pub);
        sas_material.extend_from_slice(&hi_pub);
        let sas_bytes = blake3::derive_key(SAS_CONTEXT, &sas_material);

        Ok(Self {
            leader_to_follower: ChaCha20Poly1305::new(Key::from_slice(&l2f)),
            follower_to_leader: ChaCha20Poly1305::new(Key::from_slice(&f2l)),
            sas: digits_from(&sas_bytes, SAS_DIGITS),
        })
    }

    /// The string to display for the user to compare.
    #[must_use]
    pub fn short_auth_string(&self) -> &str {
        &self.sas
    }

    fn cipher(&self, direction: KeyDirection) -> &ChaCha20Poly1305 {
        match direction {
            KeyDirection::LeaderToFollower => &self.leader_to_follower,
            KeyDirection::FollowerToLeader => &self.follower_to_leader,
        }
    }

    /// Encrypts a payload for a given direction and sequence number.
    ///
    /// `associated_data` should be the PDU header, so that an attacker cannot
    /// move a valid ciphertext to a different sequence number or message kind.
    /// Encrypting the payload while leaving the header unauthenticated would
    /// permit exactly that.
    pub fn seal(
        &self,
        direction: KeyDirection,
        seq: u32,
        associated_data: &[u8],
        plaintext: &[u8],
    ) -> Vec<u8> {
        let nonce = nonce_for(seq);
        self.cipher(direction)
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: associated_data,
                },
            )
            .expect("ChaCha20-Poly1305 encryption is infallible for valid input")
    }

    /// Decrypts a payload, verifying the header along with it.
    pub fn open(
        &self,
        direction: KeyDirection,
        seq: u32,
        associated_data: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let nonce = nonce_for(seq);
        self.cipher(direction)
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad: associated_data,
                },
            )
            .map_err(|_| CryptoError::Decrypt)
    }
}

/// Bytes the authentication tag adds to every payload.
pub const TAG_LEN: usize = 16;

/// Builds the nonce for a sequence number.
///
/// The sequence number alone suffices because each direction has its own key, so
/// a nonce can only repeat if the sequence number wraps. At u32 and ten frames a
/// second that is roughly thirteen years of continuous transmission — well past
/// any plausible session, but worth stating rather than leaving implicit.
fn nonce_for(seq: u32) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(&seq.to_le_bytes());
    nonce
}

/// Renders key material as decimal digits.
///
/// Decimal rather than hexadecimal because these get read aloud and compared by
/// a person. Hex invites confusing b with 6 and 0 with O; digits do not.
fn digits_from(bytes: &[u8], count: usize) -> String {
    let mut acc: u64 = 0;
    for b in bytes.iter().take(8) {
        acc = (acc << 8) | u64::from(*b);
    }
    let modulus = 10u64.pow(count as u32);
    format!("{:0width$}", acc % modulus, width = count)
}
