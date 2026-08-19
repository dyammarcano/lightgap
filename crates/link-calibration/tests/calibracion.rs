//! Tests de la negociación y el ajuste de perfiles.

use std::time::Duration;

use link_calibration::adaptive::{Adaptation, Aimd, GOOD_STREAK_TO_INCREASE};
use link_calibration::ladder::{Ladder, Phase, DEFAULT_MARGIN_PCT};
use link_calibration::lifecycle::{
    Lifecycle, LinkState, Transition, DEGRADE_DEBOUNCE, SILENCE_TO_DOWN,
};
use link_calibration::scoring::{best, Measurement};

/// Enlace de mentira con un techo conocido: por encima de `techo` no lee nada.
fn tasa(valor: u32, techo: u32) -> f32 {
    if valor <= techo {
        1.0
    } else {
        0.0
    }
}

fn resolver(techo: u32, min: u32, max: u32, inicio: u32) -> Option<u32> {
    let mut l = Ladder::new(min, max, inicio);
    let mut vueltas = 0;
    while l.phase() != Phase::Settled {
        vueltas += 1;
        assert!(vueltas < 200, "la escalera no converge");
        let t = tasa(l.current(), techo);
        l.record(t);
    }
    l.settled()
}

#[test]
fn la_escalera_encuentra_el_techo_con_margen() {
    // Techo 1000, margen del 15 %: se espera algo cercano a 850, y nunca por
    // encima del techo.
    let r = resolver(1000, 64, 4096, 128).expect("debería encontrar algo");
    assert!(r <= 1000, "el resultado {r} no puede superar el techo");
    assert!(r >= 500, "el resultado {r} es innecesariamente conservador");
}

#[test]
fn el_margen_deja_sitio_para_que_el_enlace_empeore() {
    let mut l = Ladder::new(64, 4096, 128);
    while l.phase() != Phase::Settled {
        l.record(tasa(l.current(), 1000));
    }
    let elegido = l.settled().unwrap();
    // El margen tiene que ser real: operar en el límite exacto significa caerse
    // en cuanto alguien mueve un portátil.
    let sin_margen = 1000;
    let esperado_maximo = sin_margen * u32::from(100 - DEFAULT_MARGIN_PCT) / 100;
    assert!(
        elegido <= esperado_maximo,
        "{elegido} no deja el margen del {DEFAULT_MARGIN_PCT} %"
    );
}

#[test]
fn converge_para_muchos_techos_distintos() {
    for techo in [70u32, 100, 250, 640, 1000, 2000, 4096] {
        let r = resolver(techo, 64, 4096, 128);
        let r = r.unwrap_or_else(|| panic!("no convergió con techo {techo}"));
        assert!(r <= techo, "con techo {techo} eligió {r}");
        assert!(
            r >= 64,
            "con techo {techo} eligió {r}, por debajo del mínimo"
        );
    }
}

#[test]
fn un_enlace_que_no_da_ni_el_minimo_no_devuelve_perfil() {
    // Techo por debajo del mínimo del rango: no hay perfil viable.
    let mut l = Ladder::new(64, 4096, 128);
    while l.phase() != Phase::Settled {
        l.record(tasa(l.current(), 10));
    }
    assert_eq!(
        l.settled(),
        None,
        "sin ningún valor que funcione no hay que inventar un perfil; hay que \
         arreglar el encuadre"
    );
}

#[test]
fn un_enlace_que_aguanta_el_maximo_se_queda_ahi() {
    let r = resolver(u32::MAX, 64, 4096, 128).expect("debería converger");
    let con_margen = 4096 * u32::from(100 - DEFAULT_MARGIN_PCT) / 100;
    assert_eq!(r, con_margen);
}

#[test]
fn la_escalera_no_lanza_sondas_sin_fin() {
    let mut l = Ladder::new(64, 4096, 128);
    while l.phase() != Phase::Settled {
        l.record(tasa(l.current(), 977));
    }
    // Duplicar hasta 4096 son ~6 pasos y bisecar unos 12 más. Nadie sostiene
    // dos portátiles enfrentados durante cien sondas.
    assert!(
        l.probes() <= 20,
        "gastó {} sondas; la calibración se haría eterna",
        l.probes()
    );
}

#[test]
fn rendirse_conserva_lo_mejor_conocido() {
    let mut l = Ladder::new(64, 4096, 128);
    l.record(1.0); // 128 va bien
    l.record(1.0); // 256 va bien
    l.give_up();
    assert_eq!(l.phase(), Phase::Settled);
    let r = l.settled().expect("debería conservar lo conocido");
    assert!(r <= 256 && r >= 64);
}

// --- puntuación -------------------------------------------------------------

#[test]
fn el_goodput_no_es_la_capacidad() {
    let grande = Measurement {
        payload_bytes: 1500,
        frames_per_second: 5.0,
        success_rate: 0.95,
        retry_rate: 0.05,
        decode_ms: 20.0,
    };
    let mediano = Measurement {
        payload_bytes: 900,
        frames_per_second: 12.0,
        success_rate: 0.98,
        retry_rate: 0.02,
        decode_ms: 12.0,
    };

    assert!(
        mediano.goodput_bps() > grande.goodput_bps(),
        "el marco mediano y rápido debería entregar más que el grande y lento"
    );
    assert!(mediano.score() > grande.score());
}

#[test]
fn la_latencia_de_decodificacion_penaliza() {
    let rapido = Measurement {
        payload_bytes: 900,
        frames_per_second: 10.0,
        success_rate: 1.0,
        retry_rate: 0.0,
        decode_ms: 5.0,
    };
    let lento = Measurement {
        decode_ms: 200.0,
        ..rapido
    };

    assert_eq!(rapido.goodput_bps(), lento.goodput_bps(), "mismo goodput");
    assert!(
        rapido.score() > lento.score() * 2.0,
        "decodificar el doble de lento que un marco entero debe penalizar fuerte"
    );
}

#[test]
fn los_reintentos_penalizan_mas_que_su_fraccion() {
    let limpio = Measurement {
        payload_bytes: 900,
        frames_per_second: 10.0,
        success_rate: 1.0,
        retry_rate: 0.0,
        decode_ms: 10.0,
    };
    let con_reintentos = Measurement {
        retry_rate: 0.3,
        ..limpio
    };
    assert!(con_reintentos.score() < limpio.score() * 0.75);
}

#[test]
fn elegir_el_mejor_descarta_los_que_no_entregan_nada() {
    let muerto = Measurement {
        payload_bytes: 2000,
        frames_per_second: 10.0,
        success_rate: 0.0,
        retry_rate: 1.0,
        decode_ms: 5.0,
    };
    let vivo = Measurement {
        payload_bytes: 300,
        frames_per_second: 8.0,
        success_rate: 0.99,
        retry_rate: 0.01,
        decode_ms: 8.0,
    };

    let elegido = best(&[("muerto", muerto), ("vivo", vivo)]).expect("hay uno vivo");
    assert_eq!(elegido.0, "vivo");

    assert_eq!(
        best::<&str>(&[]),
        None,
        "sin candidatos no hay que inventar uno"
    );
    assert_eq!(
        best(&[("muerto", muerto)]).map(|(n, _)| n),
        None,
        "un perfil que no entrega nada no es el menos malo, es un enlace roto"
    );
}

// --- ajuste continuo --------------------------------------------------------

#[test]
fn no_sube_a_la_primera_observacion_buena() {
    let mut a = Aimd::new(1000, 100, 2000, 64);
    for _ in 0..(GOOD_STREAK_TO_INCREASE - 1) {
        assert_eq!(a.observe(1.0), Adaptation::Hold);
    }
    assert_eq!(a.current(), 1000, "todavía no debería haber subido");
    assert_eq!(a.observe(1.0), Adaptation::Increase);
    assert_eq!(a.current(), 1064);
}

#[test]
fn una_mala_racha_rompe_la_buena() {
    let mut a = Aimd::new(1000, 100, 2000, 64);
    a.observe(1.0);
    a.observe(1.0);
    a.observe(0.96); // aceptable pero no excelente
    assert_eq!(a.good_streak(), 0, "la racha debe reiniciarse");
    assert_eq!(
        a.observe(1.0),
        Adaptation::Hold,
        "vuelve a empezar a contar"
    );
}

#[test]
fn baja_multiplicativamente_al_sufrir() {
    let mut a = Aimd::new(1000, 100, 2000, 64);
    assert_eq!(a.observe(0.90), Adaptation::Reduce);
    assert_eq!(a.current(), 700, "×0,7");
}

#[test]
fn se_desploma_al_romperse_el_enlace() {
    let mut a = Aimd::new(1000, 100, 2000, 64);
    assert_eq!(a.observe(0.5), Adaptation::Recover);
    assert_eq!(
        a.current(),
        490,
        "por debajo del umbral de agonía retrocede el doble: ×0,49"
    );
}

#[test]
fn subir_es_lento_y_bajar_es_rapido() {
    // La asimetría es la razón de ser del controlador; si fuera simétrico,
    // recuperarse de una degradación costaría lo mismo que provocarla.
    let mut a = Aimd::new(1000, 100, 4000, 64);
    for _ in 0..(GOOD_STREAK_TO_INCREASE * 3) {
        a.observe(1.0);
    }
    let subido = a.current();
    assert_eq!(subido, 1000 + 64 * 3);

    a.observe(0.5);
    assert!(
        a.current() < 1000,
        "una sola observación mala debe deshacer con creces tres subidas"
    );
}

#[test]
fn no_baja_por_debajo_del_minimo_ni_miente_al_hacerlo() {
    let mut a = Aimd::new(100, 100, 2000, 64);
    assert_eq!(
        a.observe(0.1),
        Adaptation::Recover,
        "un enlace roto se avisa aunque no se pueda recortar"
    );
    assert_eq!(a.current(), 100);

    // Con degradación leve y ya en el mínimo, no hay nada que reducir: decir
    // «Reduce» engañaría a quien llama.
    assert_eq!(a.observe(0.90), Adaptation::Hold);
    assert_eq!(a.current(), 100);
}

#[test]
fn no_sube_por_encima_del_maximo() {
    let mut a = Aimd::new(1990, 100, 2000, 64);
    for _ in 0..(GOOD_STREAK_TO_INCREASE * 2) {
        a.observe(1.0);
    }
    assert_eq!(a.current(), 2000);
}

// --- ciclo de vida del canal ------------------------------------------------

#[test]
fn un_canal_nace_caido() {
    let l = Lifecycle::new();
    assert_eq!(l.state(), LinkState::Down);
    assert!(!l.usable());
}

#[test]
fn el_recorrido_normal_del_canal() {
    let mut l = Lifecycle::new();
    assert_eq!(l.start_probing(), Some(Transition::ProbingStarted));
    assert_eq!(l.state(), LinkState::Probing);
    assert!(!l.usable(), "sondeando todavía no transporta nada");

    assert_eq!(l.bring_up(), Some(Transition::CameUp));
    assert!(l.usable());
    assert_eq!(l.bring_up(), None, "levantar dos veces no repite el evento");
}

#[test]
fn un_bache_corto_no_degrada() {
    let mut l = Lifecycle::new();
    l.start_probing();
    l.bring_up();

    let t0 = Duration::from_secs(1);
    assert_eq!(l.observe(t0, 0.5), None);
    assert_eq!(
        l.observe(t0 + DEGRADE_DEBOUNCE / 2, 0.5),
        None,
        "un pico de mala suerte no es una degradación"
    );
    assert_eq!(l.state(), LinkState::Up);
}

#[test]
fn una_degradacion_sostenida_si_se_declara() {
    let mut l = Lifecycle::new();
    l.start_probing();
    l.bring_up();

    let t0 = Duration::from_secs(1);
    l.observe(t0, 0.5);
    assert_eq!(
        l.observe(t0 + DEGRADE_DEBOUNCE, 0.5),
        Some(Transition::Degraded)
    );
    assert_eq!(l.state(), LinkState::Degraded);
    assert!(
        l.usable(),
        "degradado sigue sirviendo mientras entregue algo"
    );
}

#[test]
fn el_canal_se_recupera_solo() {
    let mut l = Lifecycle::new();
    l.start_probing();
    l.bring_up();

    let t0 = Duration::from_secs(1);
    l.observe(t0, 0.5);
    l.observe(t0 + DEGRADE_DEBOUNCE, 0.5);
    assert_eq!(l.state(), LinkState::Degraded);

    assert_eq!(
        l.observe(t0 + DEGRADE_DEBOUNCE * 2, 0.99),
        Some(Transition::Recovered)
    );
    assert_eq!(l.state(), LinkState::Up);
}

#[test]
fn el_silencio_prolongado_tumba_el_canal() {
    let mut l = Lifecycle::new();
    l.start_probing();
    l.bring_up();
    l.observe(Duration::from_secs(1), 1.0);

    assert_eq!(l.tick(Duration::from_secs(2)), None);
    assert_eq!(
        l.tick(Duration::from_secs(1) + SILENCE_TO_DOWN),
        Some(Transition::WentDown)
    );
    assert_eq!(l.state(), LinkState::Down);
    assert!(!l.usable());
}

#[test]
fn un_canal_caido_no_reacciona_a_observaciones() {
    let mut l = Lifecycle::new();
    assert_eq!(l.observe(Duration::from_secs(1), 1.0), None);
    assert_eq!(l.tick(Duration::from_secs(60)), None);
    assert_eq!(l.state(), LinkState::Down);
}

#[test]
fn forzar_la_caida_funciona_desde_cualquier_estado() {
    let mut l = Lifecycle::new();
    l.start_probing();
    l.bring_up();
    assert_eq!(l.force_down(), Some(Transition::WentDown));
    assert_eq!(l.force_down(), None, "no repite el evento");
}
