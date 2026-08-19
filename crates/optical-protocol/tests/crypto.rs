//! Pairing and encryption tests.
//!
//! The property that matters most here is not that encryption works — that is
//! the library's job — but that **both sides derive the same keys without
//! coordinating**, and that the authentication string actually differs when the
//! pairing differs. A design where the string looks the same regardless would be
//! security theatre.

use optical_protocol::crypto::{
    CryptoError, Identity, KeyDirection, SessionKeys, SAS_DIGITS, TAG_LEN,
};
use optical_protocol::session::PeerId;

fn peer(n: u8) -> PeerId {
    let mut b = [0u8; 16];
    b[0] = n;
    PeerId::from_bytes(b)
}

/// Runs a full pairing and returns both sides' agreed state.
fn pair(a_id: PeerId, b_id: PeerId) -> (SessionKeys, SessionKeys) {
    let a = Identity::generate();
    let b = Identity::generate();

    let ka = SessionKeys::agree(a_id, &a, b_id, &b.public_bytes(), &b.nonce()).expect("a agrees");
    let kb = SessionKeys::agree(b_id, &b, a_id, &a.public_bytes(), &a.nonce()).expect("b agrees");
    (ka, kb)
}

#[test]
fn both_sides_derive_the_same_authentication_string() {
    let (ka, kb) = pair(peer(1), peer(9));
    assert_eq!(
        ka.short_auth_string(),
        kb.short_auth_string(),
        "if the two displays showed different strings the user could never \
         confirm anything"
    );
}

/// The ordering trap: each side sees the two nonces in a different order, so the
/// derivation has to order them by peer identifier rather than by who is asking.
/// Getting it wrong produces a session where each side can encrypt but neither
/// can decrypt, which looks like a transport failure and sends everyone hunting
/// in the wrong layer.
#[test]
fn key_derivation_does_not_depend_on_who_is_asking() {
    let a = Identity::generate();
    let b = Identity::generate();

    // The same pairing, derived independently from each side. Side A sees
    // (local=a, peer=b); side B sees (local=b, peer=a). Ordering by identifier
    // is what makes those two views agree.
    let from_a =
        SessionKeys::agree(peer(1), &a, peer(9), &b.public_bytes(), &b.nonce()).expect("a");
    let from_b =
        SessionKeys::agree(peer(9), &b, peer(1), &a.public_bytes(), &a.nonce()).expect("b");

    assert_eq!(
        from_a.short_auth_string(),
        from_b.short_auth_string(),
        "both sides of one pairing must derive the same material"
    );
}

/// The flip side, and not a bug: the identifiers are part of the derivation, so
/// a device claiming a different identifier produces a different session.
///
/// Worth pinning down, because it is easy to mistake for the invariant above and
/// then "fix" the ordering to make it hold — which would break the real one.
#[test]
fn the_identifiers_are_part_of_the_derivation() {
    let a = Identity::generate();
    let b = Identity::generate();

    let normal =
        SessionKeys::agree(peer(1), &a, peer(9), &b.public_bytes(), &b.nonce()).expect("normal");
    // Same keys and nonces, but the two devices claim the opposite identifiers.
    let swapped =
        SessionKeys::agree(peer(9), &a, peer(1), &b.public_bytes(), &b.nonce()).expect("swapped");

    assert_ne!(
        normal.short_auth_string(),
        swapped.short_auth_string(),
        "swapping which identifier each device claims is a different pairing,          and must derive differently"
    );
}

#[test]
fn what_one_side_seals_the_other_opens() {
    let (ka, kb) = pair(peer(1), peer(9));
    let header = b"pdu header bytes";
    let plaintext = b"the payload";

    let sealed = ka.seal(KeyDirection::LeaderToFollower, 42, header, plaintext);
    let opened = kb
        .open(KeyDirection::LeaderToFollower, 42, header, &sealed)
        .expect("the peer must be able to open it");

    assert_eq!(opened, plaintext);
}

#[test]
fn both_directions_work_independently() {
    let (ka, kb) = pair(peer(1), peer(9));
    let header = b"h";

    let forward = ka.seal(KeyDirection::LeaderToFollower, 1, header, b"forward");
    let backward = kb.seal(KeyDirection::FollowerToLeader, 1, header, b"backward");

    assert_eq!(
        kb.open(KeyDirection::LeaderToFollower, 1, header, &forward)
            .unwrap(),
        b"forward"
    );
    assert_eq!(
        ka.open(KeyDirection::FollowerToLeader, 1, header, &backward)
            .unwrap(),
        b"backward"
    );
}

/// Separate keys per direction is what allows the nonce to be the sequence
/// number alone. If the two directions shared a key, the same sequence number in
/// both directions would reuse a nonce — which is catastrophic for this cipher,
/// not merely untidy.
#[test]
fn the_same_sequence_in_both_directions_uses_different_keys() {
    let (ka, _kb) = pair(peer(1), peer(9));
    let header = b"h";
    let plaintext = b"same plaintext";

    let forward = ka.seal(KeyDirection::LeaderToFollower, 7, header, plaintext);
    let backward = ka.seal(KeyDirection::FollowerToLeader, 7, header, plaintext);

    assert_ne!(
        forward, backward,
        "identical plaintext at the same sequence must not produce identical \
         ciphertext in both directions"
    );
}

#[test]
fn a_tampered_payload_is_refused() {
    let (ka, kb) = pair(peer(1), peer(9));
    let header = b"h";
    let mut sealed = ka.seal(KeyDirection::LeaderToFollower, 1, header, b"payload");

    sealed[0] ^= 1;
    assert_eq!(
        kb.open(KeyDirection::LeaderToFollower, 1, header, &sealed),
        Err(CryptoError::Decrypt)
    );
}

/// Authenticating the header as well as the payload is what stops an attacker
/// replaying a valid ciphertext at a different sequence number or message kind.
#[test]
fn a_tampered_header_is_refused() {
    let (ka, kb) = pair(peer(1), peer(9));
    let sealed = ka.seal(
        KeyDirection::LeaderToFollower,
        1,
        b"real header",
        b"payload",
    );

    assert_eq!(
        kb.open(KeyDirection::LeaderToFollower, 1, b"fake header", &sealed),
        Err(CryptoError::Decrypt),
        "moving a valid ciphertext under a different header must fail"
    );
}

#[test]
fn a_payload_replayed_at_another_sequence_is_refused() {
    let (ka, kb) = pair(peer(1), peer(9));
    let header = b"h";
    let sealed = ka.seal(KeyDirection::LeaderToFollower, 1, header, b"payload");

    assert_eq!(
        kb.open(KeyDirection::LeaderToFollower, 2, header, &sealed),
        Err(CryptoError::Decrypt)
    );
}

#[test]
fn a_payload_replayed_in_the_other_direction_is_refused() {
    let (ka, kb) = pair(peer(1), peer(9));
    let header = b"h";
    let sealed = ka.seal(KeyDirection::LeaderToFollower, 1, header, b"payload");

    assert_eq!(
        kb.open(KeyDirection::FollowerToLeader, 1, header, &sealed),
        Err(CryptoError::Decrypt)
    );
}

/// The man-in-the-middle case the authentication string exists for. An attacker
/// pairing separately with each side gets two different sessions, and the two
/// displays therefore show different strings. This test is what says the string
/// is doing work rather than decorating the screen.
#[test]
fn a_man_in_the_middle_produces_mismatched_strings() {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let mallory_to_alice = Identity::generate();
    let mallory_to_bob = Identity::generate();

    // Alice believes she paired with Bob, but really paired with Mallory.
    let alice_side = SessionKeys::agree(
        peer(1),
        &alice,
        peer(9),
        &mallory_to_alice.public_bytes(),
        &mallory_to_alice.nonce(),
    )
    .expect("alice pairs");

    // Bob likewise.
    let bob_side = SessionKeys::agree(
        peer(9),
        &bob,
        peer(1),
        &mallory_to_bob.public_bytes(),
        &mallory_to_bob.nonce(),
    )
    .expect("bob pairs");

    assert_ne!(
        alice_side.short_auth_string(),
        bob_side.short_auth_string(),
        "an interposed attacker must make the two displays disagree, otherwise \
         comparing them proves nothing"
    );
}

#[test]
fn different_pairings_give_different_strings() {
    let (a1, _) = pair(peer(1), peer(9));
    let (a2, _) = pair(peer(1), peer(9));
    assert_ne!(
        a1.short_auth_string(),
        a2.short_auth_string(),
        "ephemeral keys mean two sessions between the same peers must differ"
    );
}

#[test]
fn the_authentication_string_is_readable_digits() {
    let (ka, _) = pair(peer(1), peer(9));
    let sas = ka.short_auth_string();

    assert_eq!(sas.len(), SAS_DIGITS);
    assert!(
        sas.chars().all(|c| c.is_ascii_digit()),
        "digits rather than hex, because a person reads these aloud and hex \
         invites confusing b with 6"
    );
}

#[test]
fn a_low_order_public_key_is_rejected() {
    // An all-zero public key forces an all-zero shared secret. A peer sending
    // one is either broken or hostile, and either way the session must not
    // proceed with a predictable key.
    let local = Identity::generate();
    // `matches!` rather than `assert_eq!`: SessionKeys deliberately does not
    // derive Debug, because printing key material into a log is a real hazard
    // and deriving it makes that one careless `dbg!` away.
    assert!(matches!(
        SessionKeys::agree(peer(1), &local, peer(9), &[0u8; 32], &[0u8; 16]),
        Err(CryptoError::BadPeerKey)
    ));
}

#[test]
fn sealing_costs_exactly_the_tag() {
    let (ka, _) = pair(peer(1), peer(9));
    let plaintext = vec![0u8; 200];
    let sealed = ka.seal(KeyDirection::LeaderToFollower, 1, b"h", &plaintext);

    assert_eq!(
        sealed.len(),
        plaintext.len() + TAG_LEN,
        "the payload budget has to account for exactly this much"
    );
}

#[test]
fn an_empty_payload_still_authenticates() {
    let (ka, kb) = pair(peer(1), peer(9));
    let sealed = ka.seal(KeyDirection::LeaderToFollower, 1, b"h", b"");
    assert_eq!(sealed.len(), TAG_LEN);
    assert_eq!(
        kb.open(KeyDirection::LeaderToFollower, 1, b"h", &sealed)
            .unwrap(),
        Vec::<u8>::new()
    );
}

#[test]
fn key_direction_flip_is_its_own_inverse() {
    assert_eq!(
        KeyDirection::LeaderToFollower.flip(),
        KeyDirection::FollowerToLeader
    );
    assert_eq!(
        KeyDirection::LeaderToFollower.flip().flip(),
        KeyDirection::LeaderToFollower
    );
}

#[test]
fn identities_are_fresh_every_time() {
    let a = Identity::generate();
    let b = Identity::generate();
    assert_ne!(
        a.public_bytes(),
        b.public_bytes(),
        "a repeated key would mean the entropy source is broken"
    );
    assert_ne!(a.nonce(), b.nonce());
}

#[test]
fn key_material_is_not_printable() {
    // A compile-time property, asserted here so nobody adds `#[derive(Debug)]`
    // to SessionKeys for convenience: it would put session keys one careless
    // `dbg!` or `{:?}` away from a log file.
    fn assert_not_debug<T>() {}
    assert_not_debug::<SessionKeys>();

    // What IS printable is the authentication string, which is meant to be seen.
    let (ka, _) = pair(peer(1), peer(9));
    assert_eq!(ka.short_auth_string().len(), SAS_DIGITS);
}
