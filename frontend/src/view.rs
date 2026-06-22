use crate::geom::*;
use crate::util::side_color;
use leptos::prelude::*;
use sheshbesh::board::{LOCAL_MOON_EXIT, PERIMETER, cell_kind};
use sheshbesh::{CellKind, GameState, PerimeterIdx, Position, Side, checker_cell, margin_coord};

// --- Статичная отрисовка доски (без фишек) ---

pub(crate) fn cell_rect(coord: (usize, usize), class: &'static str) -> impl IntoView {
    let (r, c) = coord;
    view! {
        <rect x=c as f64 + 0.08 y=r as f64 + 0.08 width=0.84 height=0.84 rx=0.14 class=class />
    }
}

/// Клетки периметра, слоты Дома (по цвету стороны), дуги Луны и казематы Тюрьмы.
pub(crate) fn static_board(state: &GameState) -> Vec<AnyView> {
    let mut nodes: Vec<AnyView> = Vec::new();

    for abs in 0..PERIMETER {
        let p = PerimeterIdx::new(abs);
        let coord = margin_coord(p);
        match cell_kind(p) {
            CellKind::Corner => nodes.push(cell_rect(coord, "corner").into_any()),
            CellKind::Moon => nodes.push(cell_rect(coord, "moon-end").into_any()),
            CellKind::Prison => nodes.push(cell_rect(coord, "prison").into_any()),
            CellKind::HomeEntrance => match home_side(state, coord) {
                // Клетка входа в Дом активной стороны — в её цвете.
                Some(side) => {
                    let (r, c) = coord;
                    nodes.push(
                        view! {
                            <rect x=c as f64 + 0.08 y=r as f64 + 0.08 width=0.84 height=0.84 rx=0.14
                                class="home-gate" stroke=side_color(side) />
                        }
                        .into_any(),
                    );
                }
                // Дом неактивной стороны — обычная клетка (без золотой рамки).
                None => nodes.push(cell_rect(coord, "track").into_any()),
            },
            CellKind::Plain if p.local() == LOCAL_MOON_EXIT => {
                nodes.push(cell_rect(coord, "moon-end").into_any());
            }
            CellKind::Plain => nodes.push(cell_rect(coord, "track").into_any()),
        }
    }

    // Слоты Дома активных сторон — кружки в цвете игрока.
    for &side in &state.active {
        let col = side_color(side);
        for depth in 0..4u8 {
            if let Some(coord) = checker_cell(side, Position::Home { depth }) {
                let (cx, cy) = center_pt(coord);
                nodes.push(
                    view! { <circle cx=cx cy=cy r=0.34 class="home-slot" stroke=col fill=col /> }
                        .into_any(),
                );
            }
        }
    }

    // Луна: парабола, наконечник и пустые поля 1/3/6 (занятые рисует слой фишек).
    for side in Side::ALL {
        let a = moon_arc(side);
        nodes.push(view! { <path d=a.path class="moon-arc" /> }.into_any());
        nodes.push(view! { <polygon points=a.arrow class="moon-arrow" /> }.into_any());
        for f in &a.fields {
            nodes.push(
                view! {
                    <circle cx=f.pt.0 cy=f.pt.1 r=0.3 class="moon-field" />
                    <text x=f.pt.0 y=f.pt.1 class="moon-num">{f.digit.to_string()}</text>
                }
                .into_any(),
            );
        }
    }

    // Тюрьма: каземат с решёткой (фишки в нём рисует слой фишек).
    for pg in prison_geoms() {
        let (cx, cy) = pg.cage;
        // По длинной оси — вдоль стороны (1.4), по короткой — внутрь доски (1.0).
        // Длинная ось — вдоль стороны (2·CAGE_HALF_LEN), короткая — внутрь доски.
        let (hw, hh) = if pg.vertical {
            (CAGE_HALF_DEPTH, CAGE_HALF_LEN)
        } else {
            (CAGE_HALF_LEN, CAGE_HALF_DEPTH)
        };
        nodes.push(
            view! {
                <rect x=cx - hw y=cy - hh width=2.0 * hw height=2.0 * hh rx=0.1 class="cage" />
            }
            .into_any(),
        );
        for k in 0..3 {
            let off = -0.6 + f64::from(k) * 0.6;
            let (x1, y1, x2, y2) = if pg.vertical {
                (cx - 0.4, cy + off, cx + 0.4, cy + off)
            } else {
                (cx + off, cy - 0.4, cx + off, cy + 0.4)
            };
            nodes.push(view! { <line x1=x1 y1=y1 x2=x2 y2=y2 class="cage-bar" /> }.into_any());
        }
    }

    nodes
}

/// Координаты точек (1..6) на кости в сетке 3×3 (поле 100×100).
pub(crate) fn die_pips(v: u8) -> Vec<(i32, i32)> {
    let (lo, mid, hi) = (28, 50, 72);
    match v {
        1 => vec![(mid, mid)],
        2 => vec![(lo, lo), (hi, hi)],
        3 => vec![(lo, lo), (mid, mid), (hi, hi)],
        4 => vec![(lo, lo), (hi, lo), (lo, hi), (hi, hi)],
        5 => vec![(lo, lo), (hi, lo), (mid, mid), (lo, hi), (hi, hi)],
        _ => vec![(lo, lo), (hi, lo), (lo, mid), (hi, mid), (lo, hi), (hi, hi)],
    }
}

/// Грань игральной кости со значением `v` как маленький SVG.
pub(crate) fn die_face(v: u8) -> impl IntoView {
    view! {
        <svg class="die" viewBox="0 0 100 100">
            <rect x=6 y=6 width=88 height=88 rx=20 class="die-body" />
            {die_pips(v).into_iter()
                .map(|(x, y)| view! { <circle cx=x cy=y r=10 class="die-pip" /> })
                .collect_view()}
        </svg>
    }
}

/// Настоящий 3D-кубик: 6 граней (1..6), который кувыркается и встаёт гранью `v`
/// к зрителю (CSS-анимация `die-roll`; конечный поворот задаётся через `--rx/--ry`).
pub(crate) fn die3d(v: u8) -> impl IntoView {
    // Конечный поворот куба, чтобы нужная грань смотрела вперёд (одна ось на грань).
    let (rx, ry) = match v {
        1 => (0, 0),
        2 => (-90, 0),
        3 => (0, -90),
        4 => (0, 90),
        5 => (90, 0),
        _ => (0, 180), // 6
    };
    // Случайные траектория, длительность и задержка — каждая кость кувыркается
    // по-своему и останавливается не одновременно (сумма < ROLL_ANIM_MS).
    let variant = match (js_sys::Math::random() * 4.0) as u8 {
        0 => "die-roll-a",
        1 => "die-roll-b",
        2 => "die-roll-c",
        _ => "die-roll-d",
    };
    // Случайный рисунок отскоков (несколько прыжков по ходу кувырка, затухают).
    let bounce = match (js_sys::Math::random() * 3.0) as u8 {
        0 => "die-bounce-a",
        1 => "die-bounce-b",
        _ => "die-bounce-c",
    };
    // Длительность кувырка держим у самого конца окна броска (`ROLL_ANIM_MS` = 2.4с):
    // кость крутится почти всё время и встаёт на грань лишь под занавес — иначе
    // рано остановившаяся кость показывала бы результат задолго до того, как ведущий
    // объявит бросок (спойлер). Лёгкий случайный разброс длительности/задержки
    // оставляет ощущение живого броска (кости встают не строго одновременно).
    let dur = 1.9 + js_sys::Math::random() * 0.35;
    let delay = js_sys::Math::random() * 0.12;
    view! {
        // Обёртка с тем же таймингом — кость подпрыгивает по ходу вращения.
        <div
            class="die3d"
            style=format!("animation:{bounce} {dur:.3}s ease-in-out {delay:.3}s forwards")
        >
            <div
                class="cube"
                style=format!(
                    "--rx:{rx}deg;--ry:{ry}deg;\
                     animation:{variant} {dur:.3}s cubic-bezier(.3,.3,.7,1) {delay:.3}s forwards",
                )
            >
                <div class="face f-front">{die_face(1)}</div>
                <div class="face f-back">{die_face(6)}</div>
                <div class="face f-right">{die_face(3)}</div>
                <div class="face f-left">{die_face(4)}</div>
                <div class="face f-top">{die_face(2)}</div>
                <div class="face f-bottom">{die_face(5)}</div>
            </div>
        </div>
    }
}
