//! Tests de la máquina de estados de sesión.
//!
//! Lo que más se prueba aquí es el desempate. Dos aplicaciones idénticas
//! mirándose es un caso simétrico, y la simetría es exactamente lo que produce
//! bloqueos: si ninguna arranca, esperan; si arrancan las dos a la vez, se
//! pisan. Todo lo demás del protocolo depende de que esto se resuelva.

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

/// Empareja dos sesiones intercambiando lo que cada una quiera transmitir.
fn emparejar(a: &mut Session, b: &mut Session, now: Duration) {
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
fn arranca_buscando_par() {
    let s = Session::new(peer(1));
    assert_eq!(s.state(), State::Discovering);
    assert_eq!(s.role(), None);
    assert_eq!(s.peer(), None);
    assert_eq!(s.session_id(), 0, "sin par no hay sesión");
}

#[test]
fn emite_hello_mientras_busca() {
    let mut s = Session::new(peer(1));
    let pdu = s.poll_transmit().expect("debería anunciarse");
    assert_eq!(pdu.kind, PduKind::Hello);
    assert!(pdu.flags.contains(Flags::SYN));
    assert_eq!(pdu.payload, peer(1).as_bytes().to_vec());
}

#[test]
fn el_hello_se_repite_pero_no_en_cada_vuelta() {
    let mut s = Session::new(peer(1));
    assert!(s.poll_transmit().is_some(), "el primero sale ya");
    assert!(
        s.poll_transmit().is_none(),
        "no debe saturar: un QR que cambia demasiado deprisa no se engancha"
    );

    s.handle_timeout(HELLO_INTERVAL);
    assert!(s.poll_transmit().is_some(), "pasado el intervalo, otra vez");
}

/// El desempate: el `PeerId` menor dirige. Sin esto, dos instancias idénticas
/// se quedarían esperándose la una a la otra.
#[test]
fn el_identificador_menor_dirige() {
    let mut bajo = Session::new(peer(1));
    let mut alto = Session::new(peer(9));

    emparejar(&mut bajo, &mut alto, Duration::ZERO);

    assert_eq!(bajo.role(), Some(Role::Leader));
    assert_eq!(alto.role(), Some(Role::Follower));
    assert_eq!(bajo.state(), State::Peered);
    assert_eq!(alto.state(), State::Peered);
}

#[test]
fn los_dos_lados_derivan_el_mismo_identificador_de_sesion() {
    let mut a = Session::new(peer(3));
    let mut b = Session::new(peer(7));
    emparejar(&mut a, &mut b, Duration::ZERO);

    assert_ne!(a.session_id(), 0);
    assert_eq!(
        a.session_id(),
        b.session_id(),
        "se deriva de los dos identificadores, sin negociarlo"
    );
}

#[test]
fn el_identificador_de_sesion_no_depende_del_orden() {
    // La derivación tiene que ser simétrica: cada lado ve los identificadores en
    // orden distinto, y aun así deben coincidir.
    let mut a1 = Session::new(peer(3));
    let mut b1 = Session::new(peer(7));
    emparejar(&mut a1, &mut b1, Duration::ZERO);

    let mut a2 = Session::new(peer(7));
    let mut b2 = Session::new(peer(3));
    emparejar(&mut a2, &mut b2, Duration::ZERO);

    assert_eq!(a1.session_id(), a2.session_id());
}

#[test]
fn identificadores_distintos_dan_sesiones_distintas() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(2));
    emparejar(&mut a, &mut b, Duration::ZERO);

    let mut c = Session::new(peer(1));
    let mut d = Session::new(peer(3));
    emparejar(&mut c, &mut d, Duration::ZERO);

    assert_ne!(a.session_id(), c.session_id());
}

#[test]
fn descubrir_al_par_produce_un_evento_una_sola_vez() {
    let mut a = Session::new(peer(1));
    let hello = Session::new(peer(9)).poll_transmit().unwrap();

    let eventos = a.handle_incoming(&hello);
    assert_eq!(
        eventos,
        vec![Event::PeerDiscovered {
            peer: peer(9),
            role: Role::Leader
        }]
    );

    assert!(
        a.handle_incoming(&hello).is_empty(),
        "repetir el Hello del mismo par no vuelve a descubrirlo"
    );
}

/// La cámara puede encuadrar la propia pantalla, o un espejo. Verse a uno mismo
/// no es encontrar un par, y tratarlo como tal produciría una sesión consigo
/// mismo que nunca avanzaría.
#[test]
fn verse_a_uno_mismo_no_cuenta_como_par() {
    let mut s = Session::new(peer(1));
    let propio = s.poll_transmit().unwrap();

    assert!(s.handle_incoming(&propio).is_empty());
    assert_eq!(s.state(), State::Discovering);
    assert_eq!(s.peer(), None);
}

#[test]
fn un_hello_de_otra_version_se_ignora_sin_romper() {
    let mut s = Session::new(peer(1));
    let raro = Pdu {
        session_id: 0,
        kind: PduKind::Hello,
        flags: Flags::SYN,
        seq: 0,
        ack: 0,
        payload: vec![0xaa; 8], // identificador de otro tamaño
    };

    assert!(s.handle_incoming(&raro).is_empty());
    assert_eq!(
        s.state(),
        State::Discovering,
        "no debe emparejarse a ciegas"
    );
}

#[test]
fn se_sigue_anunciando_tras_encontrar_par() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    emparejar(&mut a, &mut b, Duration::ZERO);
    assert_eq!(a.state(), State::Peered);

    a.handle_timeout(HELLO_INTERVAL);
    let pdu = a.poll_transmit().expect("debe seguir anunciándose");
    assert_eq!(
        pdu.kind,
        PduKind::Hello,
        "el otro lado puede no habernos visto todavía; el descubrimiento no es \
         simétrico en el tiempo"
    );
}

#[test]
fn el_silencio_prolongado_pierde_al_par() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    emparejar(&mut a, &mut b, Duration::ZERO);

    let eventos = a.handle_timeout(PEER_TIMEOUT);
    assert_eq!(eventos, vec![Event::PeerLost]);
    assert_eq!(a.state(), State::Discovering);
    assert_eq!(a.peer(), None);
    assert_eq!(a.role(), None);
    assert_eq!(a.session_id(), 0);
}

#[test]
fn una_racha_de_perdidas_no_tumba_la_sesion() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    emparejar(&mut a, &mut b, Duration::ZERO);

    // Justo por debajo del límite: en un enlace óptico las pérdidas vienen a
    // rachas, y cortar a la primera haría que la sesión se cayera sin parar.
    let eventos = a.handle_timeout(PEER_TIMEOUT - Duration::from_millis(1));
    assert!(eventos.is_empty());
    assert_eq!(a.state(), State::Peered);
}

#[test]
fn tras_perder_al_par_se_reanuncia_de_inmediato() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    emparejar(&mut a, &mut b, Duration::ZERO);

    a.handle_timeout(PEER_TIMEOUT);
    assert!(
        a.poll_transmit().is_some(),
        "quien acaba de perder al par es quien más prisa tiene por anunciarse"
    );
}

#[test]
fn se_puede_reencontrar_al_par_despues_de_perderlo() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    emparejar(&mut a, &mut b, Duration::ZERO);
    let sesion_original = a.session_id();

    a.handle_timeout(PEER_TIMEOUT);
    assert_eq!(a.state(), State::Discovering);

    emparejar(&mut a, &mut b, PEER_TIMEOUT);
    assert_eq!(a.state(), State::Peered);
    assert_eq!(
        a.session_id(),
        sesion_original,
        "los mismos pares derivan la misma sesión"
    );
}

#[test]
fn las_capacidades_pasan_a_negociar() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    emparejar(&mut a, &mut b, Duration::ZERO);

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
fn la_calibracion_es_quien_declara_listo() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    emparejar(&mut a, &mut b, Duration::ZERO);

    assert_eq!(a.mark_ready(), vec![Event::Ready]);
    assert_eq!(a.state(), State::Active);
}

#[test]
fn no_se_puede_declarar_listo_sin_par() {
    let mut s = Session::new(peer(1));
    assert!(
        s.mark_ready().is_empty(),
        "sin par no hay nada que declarar listo"
    );
    assert_eq!(s.state(), State::Discovering);
}

#[test]
fn cerrar_avisa_al_par() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    emparejar(&mut a, &mut b, Duration::ZERO);

    assert_eq!(a.close(), vec![Event::Closed]);
    assert_eq!(a.state(), State::Closed);

    let aviso = a.poll_transmit().expect("debe avisar al otro lado");
    assert_eq!(aviso.kind, PduKind::Cancel);
    assert!(aviso.flags.contains(Flags::FIN));

    assert_eq!(b.handle_incoming(&aviso), vec![Event::Closed]);
    assert_eq!(b.state(), State::Closed);
}

#[test]
fn una_sesion_cerrada_no_reacciona_a_nada() {
    let mut a = Session::new(peer(1));
    let mut b = Session::new(peer(9));
    emparejar(&mut a, &mut b, Duration::ZERO);
    a.close();
    a.poll_transmit();

    // b ya se anunció al emparejar; hay que dejar pasar el intervalo para que
    // vuelva a hacerlo.
    b.handle_timeout(HELLO_INTERVAL);
    let hello = b.poll_transmit().expect("b vuelve a anunciarse");
    assert!(a.handle_incoming(&hello).is_empty());
    assert!(a.handle_timeout(Duration::from_secs(60)).is_empty());
    assert!(a.poll_transmit().is_none());
    assert_eq!(a.state(), State::Closed);
    assert!(a.close().is_empty(), "cerrar dos veces no repite el evento");
}
