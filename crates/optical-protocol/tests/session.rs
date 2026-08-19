//! Session state machine tests.
//!
//! What gets exercised most here is the tie-break. Two identical applications
//! facing each other is a symmetric case, and symmetry is exactly what produces
//! deadlocks: if neither starts they wait, and if both start at once they clash.
//! Everything else in the protocol depends on this being resolved.

use std::time::Duration;

use optical_protocol::session::{
    Event, PeerId, Role, Session, State, HELLO_INTERVAL, PEER_TIMEOUT,
};
use optical_protocol::wire::{Flags, Pdu, PduKind};

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
    assert_eq!(pdu.kind, PduKind::Hello);
    assert!(pdu.flags.contains(Flags::SYN));
    assert_eq!(pdu.payload, peer(1).as_bytes().to_vec());
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
fn discovering_a_peer_fires_exactly_one_event() {
    let mut a = Session::new(peer(1));
    let hello = Session::new(peer(9)).poll_transmit().unwrap();

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
