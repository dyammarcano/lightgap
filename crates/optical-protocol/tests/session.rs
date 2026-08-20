//! Session state machine tests.
//!
//! What gets exercised most here is the tie-break. Two identical applications
//! facing each other is a symmetric case, and symmetry is exactly what produces
//! deadlocks: if neither starts they wait, and if both start at once they clash.
//! Everything else in the protocol depends on this being resolved.

use std::time::Duration;

use optical_protocol::crypto::SAS_DIGITS;
use optical_protocol::session::{
    Event, PeerId, Role, Session, State, BEACON_LEN, HELLO_INTERVAL, HELLO_LEN, PAIRING_ROTATION,
    PEER_TIMEOUT,
};

use optical_protocol::wire::{Flags, Pdu, PduKind};

/// Drives two sessions until both have agreed keys.
///
/// Two round trips, not one, and that is the shape of the handshake now rather
/// than a quirk of the harness. Each side announces itself with the smallest
/// frame the protocol can express; only once someone has answered does either
/// spend modules on key material.
fn link(a: &mut Session, b: &mut Session) -> Duration {
    // Starts far ahead of anything a test is likely to have reached. The
    // sessions own their own clocks and a harness that hands one a time earlier
    // than it has already seen stops it announcing entirely — which reads as a
    // handshake that never completes rather than as the harness being wrong.
    let mut clock = PAIRING_ROTATION * 64;
    for _ in 0..8 {
        if let Some(pdu) = a.poll_transmit() {
            b.handle_incoming(&pdu);
        }
        if let Some(pdu) = b.poll_transmit() {
            a.handle_incoming(&pdu);
        }
        if a.is_paired() && b.is_paired() {
            return clock;
        }
        clock += HELLO_INTERVAL;
        a.handle_timeout(clock);
        b.handle_timeout(clock);
    }
    panic!("the two ends never paired");
}

fn peer(n: u8) -> PeerId {
    let mut b = [0u8; 16];
    b[0] = n;
    PeerId::from_bytes(b)
}

/// Pairs two sessions by exchanging whatever each wants to transmit.
fn pair(a: &mut Session, b: &mut Session, now: Duration) {
    a.handle_timeout(now);
    b.handle_timeout(now);
    if let Some(pdu) = a.poll_transmit() {
        b.handle_incoming(&pdu);
    }
    if let Some(pdu) = b.poll_transmit() {
        a.handle_incoming(&pdu);
    }
}

#[test]
fn starts_out_looking_for_a_peer() {
    let s = Session::new(peer(1));
    assert_eq!(s.state(), State::Discovering);
    assert_eq!(s.role(), None);
    assert_eq!(s.peer(), None);
    assert_eq!(s.session_id(), 0, "no peer means no session");
}

#[test]
fn announces_itself_while_searching() {
    let mut s = Session::new(peer(1));
    let pdu = s.poll_transmit().expect("should announce itself");
    assert!(pdu.flags.contains(Flags::SYN));
    assert_eq!(
        pdu.kind,
        PduKind::Beacon,
        "while searching, the smallest frame the protocol can express"
    );
    assert_eq!(pdu.payload.len(), BEACON_LEN);
    assert_eq!(
        pdu.payload,
        peer(1).as_bytes(),
        "the identifier and nothing else: a peer that cannot read this cannot \
         read anything, so there is nothing to gain by sending more"
    );

    // Once someone has answered, the link has shown it carries at least that
    // much, and the full announcement is worth its extra modules.
    let mut s = s;
    s.handle_incoming(&Session::new(peer(9)).poll_transmit().unwrap());
    s.handle_timeout(HELLO_INTERVAL);
    let pdu = s.poll_transmit().expect("should announce again");
    assert_eq!(pdu.kind, PduKind::Hello);
    assert_eq!(pdu.payload.len(), HELLO_LEN);
    assert!(
        pdu.payload[16..48].iter().any(|b| *b != 0),
        "an announcement with no key material in it would pair with anything"
    );
}

#[test]
fn hello_repeats_but_not_on_every_poll() {
    let mut s = Session::new(peer(1));
    assert!(
        s.poll_transmit().is_some(),
        "the first one goes out at once"
    );
    assert!(
        s.poll_transmit().is_none(),
        "must not saturate: a QR code that changes too fast is hard to latch onto"
    );

    s.handle_timeout(HELLO_INTERVAL);
    assert!(s.poll_transmit().is_some(), "after the interval, again");
}

/// The tie-break: the lower `PeerId` leads. Without it, two identical instances
/// would sit waiting for each other.
#[test]
fn the_lower_identifier_leads() {
    let mut low = Session::new(peer(1));
    let mut high = Session::new(peer(9));

    pair(&mut low, &mut high, Duration::ZERO);

    assert_eq!(low.role(), Some(Role::Leader));
    assert_eq!(high.role(), Some(Role::Follower));
    assert_eq!(low.state(), State::Peered);
    assert_eq!(high.state(), State::Peered);
}

#[test]
fn both_sides_derive_the_same_session_identifier() {
    let mut a = Session::new(peer(3));
    let mut b = Session::new(peer(7));
    pair(&mut a, &mut b, Duration::ZERO);

    assert_ne!(a.session_id(), 0);
    assert_eq!(
        a.session_id(),
        b.session_id(),
        "derived from both identifiers, without negotiating it"
    );
}

#[test]
fn the_session_identifier_does_not_depend_on_order() {
    // The derivation has to be symmetric: each side sees the identifiers in a
    // different order and must still agree.
    let mut a1 = Session::new(peer(3));
    let mut b1 = Session::new(peer(7));
    pair(&mut a1, &mut b1, Duration::ZERO);

    let mut a2 = Session::new(peer(7));
    let mut b2 = Session::new(peer(3));
    pair(&mut a2, &mut b2, Duration::ZERO);

    assert_eq!(a1.session_id(), a2.session_id());
}

#[test]
fn different_identifiers_give_different_sessions() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(2));
    pair(&mut a, &mut b, Duration::ZERO);

    let mut c = Session::new(peer(1));
    let mut d = Session::new(peer(3));
    pair(&mut c, &mut d, Duration::ZERO);

    assert_ne!(a.session_id(), c.session_id());
}

#[test]
fn discovering_a_peer_happens_exactly_once() {
    let mut a = Session::new(peer(1));
    let hello = Session::new(peer(9)).poll_transmit().unwrap();

    // A beacon reveals the peer and nothing more: it carries no key material,
    // so there is nothing yet to agree on.
    let events = a.handle_incoming(&hello);
    assert_eq!(
        events,
        vec![Event::PeerDiscovered {
            peer: peer(9),
            role: Role::Leader
        }]
    );

    assert!(
        a.handle_incoming(&hello).is_empty(),
        "repeating the same peer's Hello does not rediscover it"
    );
}

/// The camera may frame its own screen, or a mirror. Seeing yourself is not
/// finding a peer, and treating it as one would produce a session with itself
/// that never advances.
#[test]
fn seeing_yourself_does_not_count_as_a_peer() {
    let mut s = Session::new(peer(1));
    let own = s.poll_transmit().unwrap();

    assert!(s.handle_incoming(&own).is_empty());
    assert_eq!(s.state(), State::Discovering);
    assert_eq!(s.peer(), None);
}

#[test]
fn a_hello_from_another_version_is_ignored_without_breaking() {
    let mut s = Session::new(peer(1));
    let odd = Pdu {
        session_id: 0,
        kind: PduKind::Hello,
        flags: Flags::SYN,
        seq: 0,
        ack: 0,
        payload: vec![0xaa; 8], // identifier of a different size
    };

    assert!(s.handle_incoming(&odd).is_empty());
    assert_eq!(
        s.state(),
        State::Discovering,
        "must not pair with something it cannot parse"
    );
}

#[test]
fn keeps_announcing_after_finding_a_peer() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    pair(&mut a, &mut b, Duration::ZERO);
    assert_eq!(a.state(), State::Peered);

    a.handle_timeout(HELLO_INTERVAL);
    let pdu = a.poll_transmit().expect("must keep announcing");
    assert_eq!(
        pdu.kind,
        PduKind::Hello,
        "the other side may not have seen us yet; discovery is not symmetric in \
         time"
    );
}

#[test]
fn prolonged_silence_loses_the_peer() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    pair(&mut a, &mut b, Duration::ZERO);

    let events = a.handle_timeout(PEER_TIMEOUT);
    assert_eq!(events, vec![Event::PeerLost]);
    assert_eq!(a.state(), State::Discovering);
    assert_eq!(a.peer(), None);
    assert_eq!(a.role(), None);
    assert_eq!(a.session_id(), 0);
}

#[test]
fn a_burst_of_losses_does_not_tear_down_the_session() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    pair(&mut a, &mut b, Duration::ZERO);

    // Just under the limit: an optical link loses frames in bursts, and cutting
    // at the first burst would have the session collapsing constantly.
    let events = a.handle_timeout(PEER_TIMEOUT - Duration::from_millis(1));
    assert!(events.is_empty());
    assert_eq!(a.state(), State::Peered);
}

#[test]
fn after_losing_the_peer_it_reannounces_immediately() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    pair(&mut a, &mut b, Duration::ZERO);

    a.handle_timeout(PEER_TIMEOUT);
    assert!(
        a.poll_transmit().is_some(),
        "whoever just lost the peer is in the biggest hurry to be found again"
    );
}

#[test]
fn a_lost_peer_can_be_found_again() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    pair(&mut a, &mut b, Duration::ZERO);
    let original_session = a.session_id();

    a.handle_timeout(PEER_TIMEOUT);
    assert_eq!(a.state(), State::Discovering);

    pair(&mut a, &mut b, PEER_TIMEOUT);
    assert_eq!(a.state(), State::Peered);
    assert_eq!(
        a.session_id(),
        original_session,
        "the same pair derives the same session"
    );
}

#[test]
fn capabilities_move_the_session_to_negotiating() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    pair(&mut a, &mut b, Duration::ZERO);

    let caps = Pdu {
        session_id: a.session_id(),
        kind: PduKind::Capabilities,
        flags: Flags::NONE,
        seq: 0,
        ack: 0,
        payload: vec![1, 2, 3],
    };
    assert_eq!(a.handle_incoming(&caps), vec![Event::NegotiationStarted]);
    assert_eq!(a.state(), State::Negotiating);
}

#[test]
fn calibration_is_what_declares_readiness() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    pair(&mut a, &mut b, Duration::ZERO);

    assert_eq!(a.mark_ready(), vec![Event::Ready]);
    assert_eq!(a.state(), State::Active);
}

#[test]
fn readiness_cannot_be_declared_without_a_peer() {
    let mut s = Session::new(peer(1));
    assert!(
        s.mark_ready().is_empty(),
        "with no peer there is nothing to declare ready"
    );
    assert_eq!(s.state(), State::Discovering);
}

#[test]
fn closing_notifies_the_peer() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    pair(&mut a, &mut b, Duration::ZERO);

    assert_eq!(a.close(), vec![Event::Closed]);
    assert_eq!(a.state(), State::Closed);

    let notice = a.poll_transmit().expect("must notify the other side");
    assert_eq!(notice.kind, PduKind::Cancel);
    assert!(notice.flags.contains(Flags::FIN));

    assert_eq!(b.handle_incoming(&notice), vec![Event::Closed]);
    assert_eq!(b.state(), State::Closed);
}

#[test]
fn a_closed_session_reacts_to_nothing() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    pair(&mut a, &mut b, Duration::ZERO);
    a.close();
    a.poll_transmit();

    // b already announced itself during pairing; the interval has to elapse
    // before it will do so again.
    b.handle_timeout(HELLO_INTERVAL);
    let hello = b.poll_transmit().expect("b announces itself again");
    assert!(a.handle_incoming(&hello).is_empty());
    assert!(a.handle_timeout(Duration::from_secs(60)).is_empty());
    assert!(a.poll_transmit().is_none());
    assert_eq!(a.state(), State::Closed);
    assert!(
        a.close().is_empty(),
        "closing twice does not repeat the event"
    );
}

#[test]
fn both_sides_derive_the_same_authentication_string() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));

    let _ = link(&mut a, &mut b);

    let sas_a = a.short_auth_string().expect("a paired").to_owned();
    let sas_b = b.short_auth_string().expect("b paired").to_owned();

    // The whole defence against a man in the middle is two people comparing
    // these digits aloud. If the two ends can disagree while both believing
    // they are paired, the comparison proves nothing.
    assert_eq!(sas_a, sas_b);
    assert_eq!(sas_a.len(), SAS_DIGITS);
}

#[test]
fn the_identifier_survives_a_rotation() {
    let mut s = Session::new(peer(1));
    let first = s.poll_transmit().expect("announces").payload;

    s.handle_timeout(PAIRING_ROTATION);
    let after = s.poll_transmit().expect("announces again").payload;

    assert_eq!(
        first, after,
        "role election compares identifiers, and a peer whose identifier moved \
         would be a different peer to whoever was watching. The ephemeral key \
         rotates underneath, but a beacon does not carry one — which is rather \
         the point: there is nothing in it worth photographing."
    );
}

#[test]
fn a_peer_arriving_after_several_rotations_still_pairs() {
    let mut a = Session::new(peer(1));

    // Nobody answers for a long time, so a draws fresh keys repeatedly.
    let mut clock = PAIRING_ROTATION;
    for _ in 0..4 {
        a.handle_timeout(clock);
        let _ = a.poll_transmit();
        clock += PAIRING_ROTATION;
    }

    let mut b = Session::new(peer(9));
    let _ = link(&mut a, &mut b);

    assert_eq!(
        a.short_auth_string(),
        b.short_auth_string(),
        "whatever key a ended up holding, both ends must agree on it — a \
         rotation that left the two sides derived from different material would \
         look like a corrupt channel rather than a key mismatch"
    );
}

#[test]
fn the_pairing_code_stops_rotating_once_a_peer_is_found() {
    let mut a = Session::new(peer(1));
    let hello_b = Session::new(peer(9)).poll_transmit().unwrap();
    a.handle_incoming(&hello_b);

    assert!(a.rotation_due().is_none(), "a found peer ends the search");

    let before = a.poll_transmit().expect("still announces").payload;

    // Well past a rotation window, with the peer kept alive throughout.
    let mut t = Duration::ZERO;
    while t < PAIRING_ROTATION * 2 {
        t += PEER_TIMEOUT / 2;
        a.handle_timeout(t);
        a.handle_incoming(&hello_b);
    }

    let after = a.poll_transmit().expect("still announces").payload;
    assert_eq!(
        before, after,
        "rotating after pairing would throw away an agreement that already worked"
    );
}

#[test]
fn a_rotation_on_the_peers_side_is_recovered_from() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));

    // Pair properly first: a beacon each way, then the announcements that
    // carry keys.
    let mut clock = link(&mut a, &mut b);
    let first = a.short_auth_string().expect("a paired").to_owned();

    // b loses a, falls back to searching, and draws a new key. This is the
    // window that matters: a is still holding keys derived from material b has
    // discarded.
    clock += PEER_TIMEOUT * 2;
    b.handle_timeout(clock);
    assert!(!b.is_paired(), "b should have let go of a");

    // b finds a again and announces with its new key.
    a.handle_timeout(clock);
    b.handle_incoming(&a.poll_transmit().expect("a announces"));
    clock += HELLO_INTERVAL;
    b.handle_timeout(clock);
    let new_b = b.poll_transmit().expect("b announces again");

    // Seeing the new material, a agrees again rather than keeping keys that
    // cannot work. Without this the two ends stay silently out of step, and a
    // key mismatch on an optical link looks exactly like a dirty lens.
    a.handle_incoming(&new_b);
    let second = a.short_auth_string().expect("a still paired").to_owned();
    assert_ne!(first, second);

    // And b, once it sees a, lands on the same digits.
    clock += HELLO_INTERVAL;
    a.handle_timeout(clock);
    let hello_a = a.poll_transmit().expect("a announces");
    b.handle_incoming(&hello_a);
    assert_eq!(b.short_auth_string(), Some(second.as_str()));
}

#[test]
fn seeing_and_being_seen_are_answered_separately() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));

    // b's first announcement went out before it had seen anyone, so it carries
    // no session identifier.
    let first_b = b.poll_transmit().expect("b announces");
    assert_eq!(first_b.session_id, 0);

    a.handle_incoming(&first_b);
    assert!(a.sees_peer(), "a has read b's code");
    assert!(
        !a.peer_sees_us(),
        "b announced before it had seen anything, so nothing yet says it can \
         see a — and claiming otherwise would send someone to adjust the end \
         that is already working"
    );

    // Now b reads a, and its next announcement carries the identifier derived
    // from both of them.
    let hello_a = a.poll_transmit().expect("a announces");
    b.handle_incoming(&hello_a);
    let second_b = {
        b.handle_timeout(HELLO_INTERVAL);
        b.poll_transmit().expect("b announces again")
    };
    assert_ne!(second_b.session_id, 0);

    a.handle_incoming(&second_b);
    assert!(
        a.peer_sees_us(),
        "an identifier derived from both peers can only come from one that has \
         read this one's"
    );
}

#[test]
fn losing_the_peer_withdraws_the_claim_that_it_can_see_us() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));

    b.handle_incoming(&a.poll_transmit().expect("a announces"));
    b.handle_timeout(HELLO_INTERVAL);
    a.handle_incoming(&b.poll_transmit().expect("b announces"));
    assert!(a.peer_sees_us());

    a.handle_timeout(HELLO_INTERVAL + PEER_TIMEOUT);
    assert!(
        !a.peer_sees_us(),
        "a peer that has gone quiet is not a peer that can still see us"
    );
}

#[test]
fn each_end_reports_how_well_it_reads_the_other() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));

    a.set_read_quality(0.8);
    let _ = link(&mut a, &mut b);

    let reported = b.peer_read_quality().expect("b was told");
    assert!(
        (reported - 0.8).abs() < 0.01,
        "b should learn how well a is reading it, got {reported}"
    );

    // And it is the peer's number, not b's own: what b now knows is a's opinion
    // of b's display, which is the only figure that should size what b sends.
}

#[test]
fn a_peer_that_has_measured_nothing_is_not_a_peer_reading_at_zero() {
    let mut a = Session::new(peer(1));
    // b has just started: it has read nothing, well or badly.
    let mut b = Session::new(peer(9));

    let clock = link(&mut a, &mut b);

    assert_eq!(
        a.peer_read_quality(),
        None,
        "silence is not a measurement of zero — and the peer acts on this by          shrinking what it transmits, so the two readings would send it in          opposite directions at the moment it can least afford it"
    );

    // Once b has actually measured badly, that *is* worth acting on.
    b.set_read_quality(0.0);
    b.handle_timeout(clock + HELLO_INTERVAL);
    a.handle_incoming(&b.poll_transmit().expect("b announces again"));
    assert_eq!(a.peer_read_quality(), Some(0.0));
}

#[test]
fn the_reported_quality_is_forgotten_with_the_peer() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));

    b.set_read_quality(0.9);
    let clock = link(&mut a, &mut b);
    assert!(a.peer_read_quality().is_some());

    a.handle_timeout(clock + PEER_TIMEOUT * 2);
    assert_eq!(
        a.peer_read_quality(),
        None,
        "a figure from a peer that has gone is not a measurement of anything, \
         and sizing the next transmission from it would size it for a link \
         that no longer exists"
    );
}

#[test]
fn the_searching_frame_is_smaller_than_the_paired_one() {
    let mut a = Session::new(peer(1));
    let searching = a.poll_transmit().expect("a announces").to_vec().unwrap();

    a.handle_incoming(&Session::new(peer(9)).poll_transmit().unwrap());
    a.handle_timeout(HELLO_INTERVAL);
    let paired = a
        .poll_transmit()
        .expect("a announces again")
        .to_vec()
        .unwrap();

    assert!(
        searching.len() * 2 < paired.len(),
        "acquisition must cost far fewer bytes than what follows it — {} against \
         {}. A code's modules grow with the bytes in it and shrink to fit the \
         same screen, so this ratio is the margin the link has before anything \
         is known about what the peer can read",
        searching.len(),
        paired.len()
    );
}
