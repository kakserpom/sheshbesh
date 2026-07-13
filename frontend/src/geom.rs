use leptos::prelude::*;
use sheshbesh::board::{
    LOCAL_MOON, LOCAL_MOON_EXIT, LOCAL_PRISON_FAR, LOCAL_PRISON_NEAR, cell_kind,
};
use sheshbesh::{
    BOARD_DIM, BOARD_MARGIN, CellKind, Checker, Game, GameState, MoonField, PerimeterIdx, Position,
    Side, checker_cell, margin_coord,
};

// --- Геометрия ---

/// Центр клетки сетки `(строка, столбец)` в координатах SVG.
pub(crate) fn center_pt((row, col): (usize, usize)) -> (f64, f64) {
    (col as f64 + 0.5, row as f64 + 0.5)
}

/// Точка на квадратичной кривой Безье.
pub(crate) fn bez(p0: (f64, f64), c: (f64, f64), p2: (f64, f64), t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    (
        u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p2.0,
        u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p2.1,
    )
}

pub(crate) struct FieldGeom {
    pub(crate) field: MoonField,
    pub(crate) pt: (f64, f64),
    pub(crate) digit: char,
}

pub(crate) struct ArcGeom {
    pub(crate) path: String,
    pub(crate) arrow: String,
    pub(crate) fields: Vec<FieldGeom>,
}

/// Дуга Луны стороны `side`: парабола от входа к выходу, выгиб внутрь квадрата.
/// Параметр `t` ∈ [0,1] поля Луны на параболе (вход 0 → выход 1).
pub(crate) fn moon_field_t(field: MoonField) -> f64 {
    match field {
        MoonField::One => 0.30,
        MoonField::Three => 0.50,
        MoonField::Six => 0.72,
    }
}

/// Точка на параболе Луны стороны `side` при параметре `t` ∈ [0,1] (вход→выход).
pub(crate) fn moon_arc_point(side: Side, t: f64) -> (f64, f64) {
    let c = BOARD_DIM as f64 / 2.0;
    let p0 = center_pt(margin_coord(side.local_to_perimeter(LOCAL_MOON)));
    let p2 = center_pt(margin_coord(side.local_to_perimeter(LOCAL_MOON_EXIT)));
    let mid = ((p0.0 + p2.0) / 2.0, (p0.1 + p2.1) / 2.0);
    let dir = (c - mid.0, c - mid.1);
    let len = (dir.0 * dir.0 + dir.1 * dir.1).sqrt().max(1e-3);
    let ctrl = (mid.0 + dir.0 / len * 10.5, mid.1 + dir.1 / len * 10.5);
    bez(p0, ctrl, p2, t)
}

pub(crate) fn moon_arc(side: Side) -> ArcGeom {
    let c = BOARD_DIM as f64 / 2.0;
    let p0 = center_pt(margin_coord(side.local_to_perimeter(LOCAL_MOON)));
    let p2 = center_pt(margin_coord(side.local_to_perimeter(LOCAL_MOON_EXIT)));
    let mid = ((p0.0 + p2.0) / 2.0, (p0.1 + p2.1) / 2.0);
    let dir = (c - mid.0, c - mid.1);
    let len = (dir.0 * dir.0 + dir.1 * dir.1).sqrt().max(1e-3);
    let ctrl = (mid.0 + dir.0 / len * 10.5, mid.1 + dir.1 / len * 10.5);
    let path = format!(
        "M {:.2} {:.2} Q {:.2} {:.2} {:.2} {:.2}",
        p0.0, p0.1, ctrl.0, ctrl.1, p2.0, p2.1
    );
    let der = (2.0 * (p2.0 - ctrl.0), 2.0 * (p2.1 - ctrl.1));
    let dl = (der.0 * der.0 + der.1 * der.1).sqrt().max(1e-3);
    let (ux, uy) = (der.0 / dl, der.1 / dl);
    let (bx, by) = (p2.0 - ux * 0.3, p2.1 - uy * 0.3);
    let (px, py) = (-uy, ux);
    let arrow = format!(
        "{:.2},{:.2} {:.2},{:.2} {:.2},{:.2}",
        p2.0,
        p2.1,
        bx + px * 0.14,
        by + py * 0.14,
        bx - px * 0.14,
        by - py * 0.14
    );
    let fields = [
        (MoonField::One, 0.30, '1'),
        (MoonField::Three, 0.5, '3'),
        (MoonField::Six, 0.72, '6'),
    ]
    .into_iter()
    .map(|(field, t, digit)| FieldGeom {
        field,
        digit,
        pt: bez(p0, ctrl, p2, t),
    })
    .collect();
    ArcGeom {
        path,
        arrow,
        fields,
    }
}

pub(crate) fn moon_field_point(side: Side, field: MoonField) -> (f64, f64) {
    moon_arc(side)
        .fields
        .into_iter()
        .find(|f| f.field == field)
        .map(|f| f.pt)
        .expect("moon field")
}

/// Полудлина каземата вдоль стороны и его полуглубина внутрь доски.
pub(crate) const CAGE_HALF_LEN: f64 = 1.0;
pub(crate) const CAGE_HALF_DEPTH: f64 = 0.5;
/// Крайние слоты фишек по длинной оси каземата.
pub(crate) const CAGE_SLOT_END: f64 = 0.66;
/// Сдвиг каземата вдоль стороны в сторону Дома (чтобы не касался параболы Луны).
pub(crate) const CAGE_HOME_SHIFT: f64 = 0.5;

#[derive(Clone, Copy)]
pub(crate) struct PrisonGeom {
    pub(crate) coord: (usize, usize),
    pub(crate) cage: (f64, f64),
    /// Единичный вектор вдоль стороны в сторону Дома (длинная ось каземата).
    pub(crate) along: (f64, f64),
    /// Каземат на вертикальной стороне (вход слева/справа) — рисуется развёрнутым.
    pub(crate) vertical: bool,
}

/// Тюрьмы всех сторон: клетка-маркер на периметре и «каземат» внутри доски —
/// **вровень с клеткой Тюрьмы** (по перпендикуляру внутрь, без сдвига вдоль стороны).
pub(crate) fn prison_geoms() -> Vec<PrisonGeom> {
    let c = BOARD_DIM as f64 / 2.0;
    let mut out = Vec::new();
    for side in Side::ALL {
        let home = center_pt(margin_coord(side.entry()));
        for local in [LOCAL_PRISON_NEAR, LOCAL_PRISON_FAR] {
            let coord = margin_coord(side.local_to_perimeter(local));
            let p = center_pt(coord);
            let d = (c - p.0, c - p.1); // внутрь, к центру
            // Доминирующая ось — перпендикуляр к стороне; вторая — вдоль стороны.
            let vertical = d.0.abs() > d.1.abs();
            let (inward, along) = if vertical {
                ((d.0.signum(), 0.0), (0.0, (home.1 - p.1).signum()))
            } else {
                ((0.0, d.1.signum()), ((home.0 - p.0).signum(), 0.0))
            };
            // Вплотную к клетке (пол-клетки + полуглубина), со сдвигом вдоль стороны
            // в направлении Дома, чтобы каземат не касался параболы Луны.
            let depth = 0.5 + CAGE_HALF_DEPTH;
            let cage = (
                p.0 + inward.0 * depth + along.0 * CAGE_HOME_SHIFT,
                p.1 + inward.1 * depth + along.1 * CAGE_HOME_SHIFT,
            );
            out.push(PrisonGeom {
                coord,
                cage,
                along,
                vertical,
            });
        }
    }
    out
}

pub(crate) fn prison_geom(coord: (usize, usize)) -> Option<PrisonGeom> {
    prison_geoms().into_iter().find(|p| p.coord == coord)
}

/// Прямоугольник каземата `(x, y, ширина, высота)` для рамки-обводки (как у резерва).
/// Длинная ось — вдоль стороны (`CAGE_HALF_LEN`), короткая — вглубь (`CAGE_HALF_DEPTH`).
pub(crate) fn cage_rect(coord: (usize, usize)) -> Option<(f64, f64, f64, f64)> {
    let g = prison_geom(coord)?;
    let (hw, hh) = if g.vertical {
        (CAGE_HALF_DEPTH, CAGE_HALF_LEN)
    } else {
        (CAGE_HALF_LEN, CAGE_HALF_DEPTH)
    };
    Some((g.cage.0 - hw, g.cage.1 - hh, 2.0 * hw, 2.0 * hh))
}

/// Точка слота `k` из `n` по длинной оси каземата (центрирована, симметрична).
pub(crate) fn prison_slot_point(g: &PrisonGeom, k: usize, n: usize) -> (f64, f64) {
    let off = if n > 1 {
        (k as f64 - (n as f64 - 1.0) / 2.0) * (2.0 * CAGE_SLOT_END / (n as f64 - 1.0))
    } else {
        0.0
    };
    (g.cage.0 + g.along.0 * off, g.cage.1 + g.along.1 * off)
}

/// Индекс слота фишки-пленника `owner` (по порядку активных сторон) и их число.
pub(crate) fn prison_slot(state: &GameState, owner: Side) -> (usize, usize) {
    let n = state.active.len().max(1);
    let k = state.active.iter().position(|&s| s == owner).unwrap_or(0);
    (k, n)
}

/// Шаг разноса разноцветных фишек на угловой клетке и на поле Луны (наружу).
pub(crate) const CORNER_GAP: f64 = 0.6;
pub(crate) const MOON_GAP: f64 = 0.5;

/// Точка `k`-го слота: от базовой точки `base` наружу от центра доски с шагом `gap`
/// (`k == 0` — на самой точке). Одноцветные совпадают (рисуются со счётчиком).
pub(crate) fn outward_slot(base: (f64, f64), k: usize, gap: f64) -> (f64, f64) {
    let c = BOARD_DIM as f64 / 2.0;
    let d = (base.0 - c, base.1 - c);
    let len = (d.0 * d.0 + d.1 * d.1).sqrt().max(1e-3);
    let off = k as f64 * gap;
    (base.0 + d.0 / len * off, base.1 + d.1 / len * off)
}

/// Абсолютная клетка-угол, на которой стоит фишка (`None`, если не на углу).
/// Угол — единственная (кроме Тюрьмы) клетка, где уживаются разные фишки.
pub(crate) fn checker_corner(owner: Side, pos: Position) -> Option<PerimeterIdx> {
    if let Position::OnTrack { progress } = pos {
        let abs = owner.entry().advance(progress as usize);
        if cell_kind(abs) == CellKind::Corner {
            return Some(abs);
        }
    }
    None
}

/// Активные стороны, у которых есть фишка на угловой клетке `abs` (в их порядке).
pub(crate) fn corner_sides(state: &GameState, abs: PerimeterIdx) -> Vec<Side> {
    state
        .active
        .iter()
        .copied()
        .filter(|&s| {
            state
                .checkers()
                .iter()
                .any(|c| c.owner == s && checker_corner(c.owner, c.pos) == Some(abs))
        })
        .collect()
}

/// Точка слота `k` фишки на угловой клетке (как в Тюрьме, но разнос — наружу).
pub(crate) fn corner_slot_point(coord: (usize, usize), k: usize) -> (f64, f64) {
    outward_slot(center_pt(coord), k, CORNER_GAP)
}

/// Единичный вектор «наружу» (перпендикуляр к стороне доски) для клетки периметра `coord`
/// (координаты сетки `margin_coord`): от того края квадрата, на котором лежит клетка.
fn perp_outward((row, col): (usize, usize)) -> (f64, f64) {
    let lo = BOARD_MARGIN;
    let hi = BOARD_DIM - BOARD_MARGIN - 1;
    if row == lo {
        (0.0, -1.0) // верхняя сторона (C) → вверх
    } else if row == hi {
        (0.0, 1.0) // нижняя (A) → вниз
    } else if col == lo {
        (-1.0, 0.0) // левая (D) → влево
    } else {
        (1.0, 0.0) // правая (B) → вправо
    }
}

/// Точка слота `k` фишки, СТОЯЩЕЙ на клетке Тюрьмы: разнос как на углу (одноцветные —
/// в один кружок со счётчиком), но строго **перпендикулярно стороне** (наружу), без
/// диагонального наклона «к центру», как у углов — клетка Тюрьмы лежит на стороне.
pub(crate) fn prison_cell_slot_point(coord: (usize, usize), k: usize) -> (f64, f64) {
    let base = center_pt(coord);
    let (dx, dy) = perp_outward(coord);
    let off = k as f64 * CORNER_GAP;
    (base.0 + dx * off, base.1 + dy * off)
}

/// Абсолютная клетка-Тюрьма, на которой СТОИТ (OnTrack) фишка — освобождённая
/// «зашёл-вышел» или проходящая. Клетка Тюрьмы, как и угол, допускает совместное
/// стояние разных фишек (без съедания), а одноцветные группируются со счётчиком.
pub(crate) fn checker_on_prison(owner: Side, pos: Position) -> Option<PerimeterIdx> {
    if let Position::OnTrack { progress } = pos {
        let abs = owner.entry().advance(progress as usize);
        if cell_kind(abs) == CellKind::Prison {
            return Some(abs);
        }
    }
    None
}

/// Активные стороны с фишкой, СТОЯЩЕЙ на клетке Тюрьмы `abs` (в их порядке).
pub(crate) fn prison_cell_sides(state: &GameState, abs: PerimeterIdx) -> Vec<Side> {
    state
        .active
        .iter()
        .copied()
        .filter(|&s| {
            state
                .checkers()
                .iter()
                .any(|c| c.owner == s && checker_on_prison(c.owner, c.pos) == Some(abs))
        })
        .collect()
}

/// Активные стороны с фишкой на поле `field` Луны стороны `side` (в их порядке).
pub(crate) fn moon_sides(state: &GameState, side: Side, field: MoonField) -> Vec<Side> {
    state
        .active
        .iter()
        .copied()
        .filter(|&s| {
            state
                .checkers()
                .iter()
                .any(|c| c.owner == s && c.pos == Position::Moon { side, field })
        })
        .collect()
}

/// Точка слота `k` фишки на поле Луны: разнос как на углу. Поля `1`/`6` (у концов
/// дуги) разносятся наружу от центра, а поле `3` (на вершине дуги, у центра) — к
/// центру: иначе стопка налезала бы на сторону Дома.
pub(crate) fn moon_slot_point(side: Side, field: MoonField, k: usize) -> (f64, f64) {
    let gap = if field == MoonField::Three {
        -MOON_GAP
    } else {
        MOON_GAP
    };
    outward_slot(moon_field_point(side, field), k, gap)
}

/// Все три поля Луны (по порядку прохождения).
pub(crate) const MOON_FIELDS: [MoonField; 3] = [MoonField::One, MoonField::Three, MoonField::Six];

/// Бейдж-счётчик одноцветной стопки: стабильный ключ (индекс первой фишки группы —
/// чтобы цифра анимировалась вместе с кружком), точка слота и число.
#[derive(Clone, Copy)]
pub(crate) struct Badge {
    pub(crate) key: usize,
    pub(crate) pt: (f64, f64),
    pub(crate) count: usize,
}

/// Первый индекс и число фишек, удовлетворяющих предикату.
pub(crate) fn first_and_count(
    state: &GameState,
    pred: impl Fn(&Checker) -> bool,
) -> (Option<usize>, usize) {
    let mut first = None;
    let mut count = 0;
    for (i, c) in state.checkers().iter().enumerate() {
        if pred(c) {
            count += 1;
            first.get_or_insert(i);
        }
    }
    (first, count)
}

/// Бейджи-счётчики одноцветных стопок (Тюрьма, углы, Луна): одноцветные фишки
/// накладываются в один кружок, поверх которого рисуется цифра (>1). Ключ бейджа —
/// индекс первой фишки группы, чтобы при анимации цифра «ехала» вместе с кружком,
/// а не телепортировалась (keyed `<For>` + CSS-переход `transform`).
pub(crate) fn stack_counts(state: &GameState) -> Vec<Badge> {
    let mut out = Vec::new();
    let mut push = |first: Option<usize>, pt, count| {
        if count > 1 {
            out.push(Badge {
                key: first.expect("группа непуста"),
                pt,
                count,
            });
        }
    };
    // Тюрьмы: одноцветные пленники в каземате.
    for pg in prison_geoms() {
        for &side in &state.active {
            let (first, count) = first_and_count(state, |c| {
                c.owner == side
                    && matches!(c.pos, Position::Prison { .. })
                    && checker_cell(c.owner, c.pos) == Some(pg.coord)
            });
            let (k, n) = prison_slot(state, side);
            push(first, prison_slot_point(&pg, k, n), count);
        }
    }
    // Углы: одноцветные фишки на одной угловой клетке.
    for &abs in &[0usize, 18, 36, 54] {
        let abs = PerimeterIdx::new(abs);
        let coord = margin_coord(abs);
        for (k, &side) in corner_sides(state, abs).iter().enumerate() {
            let (first, count) = first_and_count(state, |c| {
                c.owner == side && checker_corner(c.owner, c.pos) == Some(abs)
            });
            push(first, corner_slot_point(coord, k), count);
        }
    }
    // Клетки Тюрьмы: одноцветные СТОЯЩИЕ на клетке (освобождённые) фишки — со счётчиком.
    for side in Side::ALL {
        for local in [LOCAL_PRISON_NEAR, LOCAL_PRISON_FAR] {
            let abs = side.local_to_perimeter(local);
            let coord = margin_coord(abs);
            for (k, &owner) in prison_cell_sides(state, abs).iter().enumerate() {
                let (first, count) = first_and_count(state, |c| {
                    c.owner == owner && checker_on_prison(c.owner, c.pos) == Some(abs)
                });
                push(first, prison_cell_slot_point(coord, k), count);
            }
        }
    }
    // Поля Луны: одноцветные фишки на одном поле.
    for side in Side::ALL {
        for field in MOON_FIELDS {
            for (k, &owner) in moon_sides(state, side, field).iter().enumerate() {
                let (first, count) = first_and_count(state, |c| {
                    c.owner == owner && c.pos == Position::Moon { side, field }
                });
                push(first, moon_slot_point(side, field, k), count);
            }
        }
    }
    out
}

/// Реактивный бейдж-счётчик стопки с ключом `key`. Позиция и число читаются из
/// **текущего** состояния (а не из снимка `<For>`), поэтому при движении фишек
/// цифра едет вместе с кружком (CSS-переход `transform`), а не остаётся на месте.
pub(crate) fn badge_view(game: RwSignal<Game>, key: usize) -> impl IntoView {
    let cur = move || {
        stack_counts(&game.get().state)
            .into_iter()
            .find(|b| b.key == key)
    };
    view! {
        <text class="cage-count"
            style=move || cur().map_or(String::new(), |b| {
                format!("transform:translate({:.3}px,{:.3}px)", b.pt.0, b.pt.1)
            })>
            {move || cur().map_or(String::new(), |b| b.count.to_string())}
        </text>
    }
}

// Зоны снаружи доски напротив клетки входа Дома (выносы наружу, в клетках):
// резерв ближе всего, плен дальше, бросок костей — ещё дальше.
pub(crate) const RESERVE_OUT: f64 = 1.0;
pub(crate) const CAPTURED_OUT: f64 = 1.85;
pub(crate) const DICE_OUT: f64 = 2.75;
pub(crate) const RESERVE_GAP: f64 = 0.64;
/// Запас viewBox под выносные зоны снаружи доски (в клетках).
pub(crate) const RESERVE_PAD: f64 = 3.3;

/// Единичные оси у клетки входа Дома `side`: наружу (перпендикуляр к стороне) и
/// вдоль стороны.
pub(crate) fn reserve_axes(side: Side) -> ((f64, f64), (f64, f64)) {
    let c = BOARD_DIM as f64 / 2.0;
    let home = center_pt(margin_coord(side.entry()));
    let d = (home.0 - c, home.1 - c); // наружу: от центра к Дому
    if d.0.abs() > d.1.abs() {
        ((d.0.signum(), 0.0), (0.0, 1.0))
    } else {
        ((0.0, d.1.signum()), (1.0, 0.0))
    }
}

/// Центр зоны на выносе `out` наружу от клетки входа Дома стороны.
pub(crate) fn outside_anchor(side: Side, out_dist: f64) -> (f64, f64) {
    let home = center_pt(margin_coord(side.entry()));
    let (out, _) = reserve_axes(side);
    (home.0 + out.0 * out_dist, home.1 + out.1 * out_dist)
}

/// Точка `k`-й из `n` фишек в выносном ряду стороны (вдоль стороны, центрирован).
pub(crate) fn outside_point(side: Side, out_dist: f64, k: usize, n: usize) -> (f64, f64) {
    let a = outside_anchor(side, out_dist);
    let (_, along) = reserve_axes(side);
    let off = (k as f64 - (n.max(1) as f64 - 1.0) / 2.0) * RESERVE_GAP;
    (a.0 + along.0 * off, a.1 + along.1 * off)
}

/// Сторона-владелец клетки Дома (вход или слот) среди активных, иначе `None`.
pub(crate) fn home_side(state: &GameState, coord: (usize, usize)) -> Option<Side> {
    state.active.iter().copied().find(|&s| {
        margin_coord(s.entry()) == coord
            || (0..4u8).any(|d| checker_cell(s, Position::Home { depth: d }) == Some(coord))
    })
}

/// «Гнездо» фишки — для группировки наложенных на обычной клетке (союзные фишки):
/// угол/Луна/Тюрьма имеют собственную раскладку, сюда не попадают.
#[derive(PartialEq, Eq)]
pub(crate) enum Slot {
    Cell((usize, usize)),
    Off,
}

pub(crate) fn slot_key(pos: Position, owner: Side) -> Slot {
    match pos {
        Position::Reserve | Position::Captured { .. } => Slot::Off,
        _ => checker_cell(owner, pos).map_or(Slot::Off, Slot::Cell),
    }
}

/// Координаты фишки `i` на доске и видимость (вне доски → невидима, в лотке).
pub(crate) fn checker_xy(state: &GameState, i: usize) -> (f64, f64, bool) {
    let ch = state.checkers()[i];
    // Пленники одной стороны делят слот (накладываются в один кружок со счётчиком),
    // а разные цвета разнесены по длинной оси каземата — без диагональной стопки.
    if let Position::Prison { .. } = ch.pos {
        let coord = checker_cell(ch.owner, ch.pos).expect("prison cell");
        let g = prison_geom(coord).expect("prison geom");
        let (k, n) = prison_slot(state, ch.owner);
        let (x, y) = prison_slot_point(&g, k, n);
        return (x, y, true);
    }
    // Резерв — видимый ряд снаружи доски у Дома (4 фикс. слота = индекс среди своих).
    if ch.pos == Position::Reserve {
        let k = (0..i)
            .filter(|&j| state.checkers()[j].owner == ch.owner)
            .count();
        let (x, y) = outside_point(ch.owner, RESERVE_OUT, k, 4);
        return (x, y, true);
    }
    // Плен — у Дома ЗАХВАТЧИКА (его «трофеи»), цвет фишки — владельца. Ряд дальше
    // резерва, центрирован по числу пленённых этим захватчиком.
    if let Position::Captured { captor } = ch.pos {
        let held_by = |p: Position| matches!(p, Position::Captured { captor: c } if c == captor);
        let n = state.checkers().iter().filter(|c| held_by(c.pos)).count();
        let k = (0..i).filter(|&j| held_by(state.checkers()[j].pos)).count();
        let (x, y) = outside_point(captor, CAPTURED_OUT, k, n);
        return (x, y, true);
    }
    // Угол — единственная (кроме Тюрьмы) клетка периметра с разными фишками: рисуем
    // как в Тюрьме (одноцветные — в один кружок со счётчиком), но цвета — НАРУЖУ.
    if let Some(abs) = checker_corner(ch.owner, ch.pos) {
        let coord = checker_cell(ch.owner, ch.pos).expect("corner cell");
        let sides = corner_sides(state, abs);
        let k = sides.iter().position(|&s| s == ch.owner).unwrap_or(0);
        let (x, y) = corner_slot_point(coord, k);
        return (x, y, true);
    }
    // Клетка Тюрьмы со СТОЯЩИМИ на ней фишками (освобождённые «зашёл-вышел») — как на
    // углу: одноцветные накладываются в один кружок, разные цвета разнесены наружу.
    if let Some(abs) = checker_on_prison(ch.owner, ch.pos) {
        let coord = checker_cell(ch.owner, ch.pos).expect("prison cell");
        let sides = prison_cell_sides(state, abs);
        let k = sides.iter().position(|&s| s == ch.owner).unwrap_or(0);
        let (x, y) = prison_cell_slot_point(coord, k);
        return (x, y, true);
    }
    // Поле Луны — тоже стопка из разных фишек: так же, как на углу.
    if let Position::Moon { side, field } = ch.pos {
        let sides = moon_sides(state, side, field);
        let k = sides.iter().position(|&s| s == ch.owner).unwrap_or(0);
        let (x, y) = moon_slot_point(side, field, k);
        return (x, y, true);
    }
    // На клетке (в т.ч. при проходе сквозь клетку Тюрьмы) — по центру клетки.
    // В каземат попадают только настоящие пленники (обработаны выше).
    let coord = checker_cell(ch.owner, ch.pos).expect("on-board cell");
    let (base, visible) = (center_pt(coord), true);
    let key = slot_key(ch.pos, ch.owner);
    let rank = (0..i)
        .filter(|&j| slot_key(state.checkers()[j].pos, state.checkers()[j].owner) == key)
        .count();
    (
        base.0 + rank as f64 * 0.16,
        base.1 - rank as f64 * 0.16,
        visible,
    )
}

/// Точка отрисовки фишки `i` с учётом покадрового переопределения `pts` (движение по
/// параболе Луны); если переопределения нет — обычная `checker_xy`.
pub(crate) fn anim_xy(state: &GameState, i: usize, pts: &[(usize, f64, f64)]) -> (f64, f64, bool) {
    if let Some(&(_, x, y)) = pts.iter().find(|p| p.0 == i) {
        (x, y, true)
    } else {
        checker_xy(state, i)
    }
}
