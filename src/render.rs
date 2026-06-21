//! Текстовое представление доски: ASCII-квадрат периметра плюс панель
//! «вне доски» (резерв, Дом, Луна, плен).
//!
//! Периметр раскладывается в квадрат против часовой стрелки: сторона A — низ
//! (слева направо), B — правый край (снизу вверх), C — верх (справа налево),
//! D — левый край (сверху вниз). Углы — общие (нижне-левый = индекс 0).
//!
//! Клетка периметра — токен из 2 символов: для пустой это landmark
//! (`+` угол, `M` Луна, `J` Тюрьма, `h` вход в Дом, `.` обычная), для занятой —
//! буква стороны и число фишек (`A2`), либо буквы сторон при совместном стоянии
//! (углы/Тюрьма), напр. `AC`.

use crate::board::{
    CellKind, HOME_DEPTH, LOCAL_MOON, LOCAL_MOON_EXIT, PERIMETER, PerimeterIdx, SIDE_LEN, Side,
    cell_kind,
};
use crate::state::{GameState, MoonField, Position};

/// Координата (строка, столбец) клетки периметра в квадратной сетке `SIDE_LEN`².
pub(crate) fn cell_coord(abs: usize) -> (usize, usize) {
    let n = SIDE_LEN - 1; // 18
    let side = abs / n; // 0..3
    let local = abs % n; // 0..17
    match side {
        0 => (SIDE_LEN - 1, local),                // A — низ, слева направо
        1 => (SIDE_LEN - 1 - local, SIDE_LEN - 1), // B — правый край, снизу вверх
        2 => (0, SIDE_LEN - 1 - local),            // C — верх, справа налево
        _ => (local, 0),                           // D — левый край, сверху вниз
    }
}

/// Landmark-символ пустой клетки.
pub(crate) fn landmark(abs: PerimeterIdx) -> char {
    match cell_kind(abs) {
        CellKind::Corner => '+',
        CellKind::Moon => 'M',
        CellKind::Prison => 'J',
        CellKind::HomeEntrance => 'h',
        CellKind::Plain => '.',
    }
}

/// Стороны фишек, стоящих на клетке периметра `abs` (с повторами).
pub(crate) fn owners_on(state: &GameState, abs: PerimeterIdx) -> Vec<Side> {
    state
        .checkers
        .iter()
        .filter(|c| c.pos.perimeter_cell(c.owner) == Some(abs))
        .map(|c| c.owner)
        .collect()
}

/// Ширина полей вокруг квадрата для «дописывания» лишних фишек наружу.
pub const BOARD_MARGIN: usize = 3;

/// Сторона квадрата сетки глифов (квадрат периметра плюс поля).
pub const BOARD_DIM: usize = SIDE_LEN + 2 * BOARD_MARGIN;

/// Ручной масштаб из `SHESHBESH_SCALE` (1..4), если задан, — переопределяет
/// автоподбор. Клетка занимает `2*scale` колонок × `scale` строк (так доска
/// остаётся квадратной: символ терминала примерно вдвое выше своей ширины).
#[cfg(feature = "tui")]
pub(crate) fn manual_scale() -> Option<usize> {
    std::env::var("SHESHBESH_SCALE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|s| s.clamp(1, 4))
}

/// Соотношение «высота/ширина» символа терминала. Берётся из реального размера
/// ячейки в пикселях (`crossterm::window_size`); при недоступности — 2.0.
/// `SHESHBESH_ASPECT` переопределяет.
#[cfg(feature = "tui")]
fn cell_aspect() -> f64 {
    if let Some(a) = std::env::var("SHESHBESH_ASPECT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        && a > 0.0
    {
        return a.clamp(1.0, 4.0);
    }
    if let Ok(ws) = crossterm::terminal::window_size()
        && ws.width > 0
        && ws.height > 0
        && ws.columns > 0
        && ws.rows > 0
    {
        let char_w = f64::from(ws.width) / f64::from(ws.columns);
        let char_h = f64::from(ws.height) / f64::from(ws.rows);
        if char_w > 0.0 {
            return (char_h / char_w).clamp(1.0, 4.0);
        }
    }
    2.0
}

/// Размер клетки доски `(колонок, строк)` под область `inner_width × inner_height`
/// (без рамки): доска остаётся физически квадратной (`колонок/строк ≈ аспект`) и
/// влезает по обеим осям. `SHESHBESH_SCALE` задаёт строки вручную.
#[cfg(feature = "tui")]
pub(crate) fn cell_size(inner_width: u16, inner_height: u16) -> (usize, usize) {
    let aspect = cell_aspect();
    let cols = |rows: usize| ((aspect * rows as f64).round() as usize).max(1);
    if let Some(rows) = manual_scale() {
        return (cols(rows), rows);
    }
    let by_height = (inner_height as usize / BOARD_DIM).max(1);
    let by_width = ((f64::from(inner_width) / (BOARD_DIM as f64 * aspect)).floor() as usize).max(1);
    let rows = by_height.min(by_width).clamp(1, 4);
    (cols(rows), rows)
}

/// Глиф одной ячейки сетки доски (рендер-независимый).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    /// Пусто (поле/внутренность квадрата).
    Empty,
    /// Пустая клетка периметра — её landmark-символ.
    Landmark(char),
    /// Элемент Луны: вход `M`, выход `m` или поле дорожки `1`/`3`/`6`.
    Moon(char),
    /// Пустая клетка Тюрьмы (`J`).
    Prison(char),
    /// Маркер фишки стороны.
    Checker(Side),
    /// Пленённая фишка стороны (сидит в Тюрьме).
    Captive(Side),
    /// Переполнение: фишек на клетке больше, чем влезает наружу.
    Overflow,
}

/// Координата базовой ячейки сетки глифов для клетки периметра `abs`.
pub fn margin_coord(abs: PerimeterIdx) -> (usize, usize) {
    let (r, c) = cell_coord(abs.get());
    (r + BOARD_MARGIN, c + BOARD_MARGIN)
}

/// Ячейка сетки `(строка, столбец)`, где рисуется фишка владельца `owner` в
/// позиции `pos` (та же раскладка, что в [`board_glyphs`]). `None` — фишка вне
/// доски (резерв или плен). Полезно GUI для сопоставления кликов и ходов.
pub fn checker_cell(owner: Side, pos: Position) -> Option<(usize, usize)> {
    match pos {
        Position::OnTrack { .. } | Position::Prison { .. } => {
            pos.perimeter_cell(owner).map(margin_coord)
        }
        Position::Home { depth } => Some(inward_cell(owner.entry(), depth as usize + 1)),
        Position::Moon { side, field } => Some(inward_cell(
            side.local_to_perimeter(LOCAL_MOON),
            moon_rank(field),
        )),
        Position::Reserve | Position::Captured { .. } => None,
    }
}

/// Направление «наружу» от клетки на краю квадрата (углы — по вертикали).
fn outward(r: usize, c: usize) -> (isize, isize) {
    if r == 0 {
        (-1, 0) // верх — вверх
    } else if r == SIDE_LEN - 1 {
        (1, 0) // низ — вниз
    } else if c == 0 {
        (0, -1) // левый край — влево
    } else {
        (0, 1) // правый край — вправо
    }
}

/// Ячейка на `k` шагов внутрь квадрата от клетки периметра `abs` (Дом/Луна).
fn inward_cell(abs: PerimeterIdx, k: usize) -> (usize, usize) {
    let (r, c) = cell_coord(abs.get());
    let (dr, dc) = outward(r, c);
    let pr = (r + BOARD_MARGIN) as isize - dr * k as isize;
    let pc = (c + BOARD_MARGIN) as isize - dc * k as isize;
    (pr as usize, pc as usize)
}

/// Ранг поля Луны (1→1, 3→2, 6→3) — шаг внутрь от клетки-входа Луны.
fn moon_rank(field: MoonField) -> usize {
    match field {
        MoonField::One => 1,
        MoonField::Three => 2,
        MoonField::Six => 3,
    }
}

/// Строит `BOARD_DIM`² сетку глифов. Фишки периметра — на клетках (лишние
/// «дописываются» наружу за край, сверх — `Overflow`). Внутрь квадрата рисуются
/// Дом (4 слота `o` от входа активных сторон) и поля Луны (`1/3/6` от входа на
/// Луну каждой стороны); занятые слоты показываются маркером фишки.
pub fn board_glyphs(state: &GameState) -> Vec<Vec<Glyph>> {
    let mut grid = vec![vec![Glyph::Empty; BOARD_DIM]; BOARD_DIM];
    let shift = |base: usize, d: isize, k: isize| (base as isize + d * k) as usize;

    for abs in 0..PERIMETER {
        let p = PerimeterIdx::new(abs);
        let (r, c) = cell_coord(abs);
        let (pr, pc) = (r + BOARD_MARGIN, c + BOARD_MARGIN);
        // Глифы фишек на клетке (пленённые — отдельным глифом).
        let occupants: Vec<Glyph> = state
            .checkers
            .iter()
            .filter(|ch| ch.pos.perimeter_cell(ch.owner) == Some(p))
            .map(|ch| {
                if matches!(ch.pos, Position::Prison { .. }) {
                    Glyph::Captive(ch.owner)
                } else {
                    Glyph::Checker(ch.owner)
                }
            })
            .collect();

        grid[pr][pc] = match occupants.first() {
            Some(&g) => g,
            None if cell_kind(p) == CellKind::Moon => Glyph::Moon('M'),
            None if cell_kind(p) == CellKind::Prison => Glyph::Prison('J'),
            None if p.local() == LOCAL_MOON_EXIT => Glyph::Moon('m'),
            None => Glyph::Landmark(landmark(p)),
        };

        let (dr, dc) = outward(r, c);
        for (k, &g) in occupants.iter().skip(1).enumerate() {
            let slot = k + 1;
            if slot > BOARD_MARGIN {
                grid[shift(pr, dr, BOARD_MARGIN as isize)][shift(pc, dc, BOARD_MARGIN as isize)] =
                    Glyph::Overflow;
                break;
            }
            grid[shift(pr, dr, slot as isize)][shift(pc, dc, slot as isize)] = g;
        }
    }

    // Пустые внутренние слоты: поля Луны (все стороны) и Дом (активные стороны).
    for side in Side::ALL {
        let moon = side.local_to_perimeter(LOCAL_MOON);
        for (rank, digit) in [(1, '1'), (2, '3'), (3, '6')] {
            let (r, c) = inward_cell(moon, rank);
            grid[r][c] = Glyph::Moon(digit);
        }
    }
    for &side in &state.active {
        for depth in 0..HOME_DEPTH {
            let (r, c) = inward_cell(side.entry(), depth + 1);
            grid[r][c] = Glyph::Landmark('o');
        }
    }

    // Фишки в Доме и на Луне — поверх слотов.
    for ch in &state.checkers {
        if matches!(ch.pos, Position::Home { .. } | Position::Moon { .. })
            && let Some((r, c)) = checker_cell(ch.owner, ch.pos)
        {
            grid[r][c] = Glyph::Checker(ch.owner);
        }
    }

    grid
}

/// Двухсимвольный токен клетки.
fn token(state: &GameState, abs: PerimeterIdx) -> String {
    let owners = owners_on(state, abs);
    if owners.is_empty() {
        return format!("{} ", landmark(abs));
    }
    let mut distinct: Vec<Side> = Vec::new();
    for &s in &owners {
        if !distinct.contains(&s) {
            distinct.push(s);
        }
    }
    if distinct.len() == 1 {
        // Одна сторона: буква + число фишек.
        format!("{}{}", distinct[0].letter(), owners.len().min(9))
    } else {
        // Совместное стояние (угол/Тюрьма): буквы сторон, максимум две.
        let s: String = distinct.iter().take(2).map(|s| s.letter()).collect();
        s
    }
}

/// Рисует квадрат периметра.
fn perimeter(state: &GameState) -> String {
    let mut grid = vec![vec![String::from("  "); SIDE_LEN]; SIDE_LEN];
    for abs in 0..PERIMETER {
        let (r, c) = cell_coord(abs);
        grid[r][c] = token(state, PerimeterIdx::new(abs));
    }
    grid.iter()
        .map(|row| row.join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Панель «вне доски» по каждой активной стороне.
fn off_board(state: &GameState) -> String {
    let mut lines = Vec::new();
    for &side in &state.active {
        let own = || state.checkers.iter().filter(move |c| c.owner == side);

        let reserve = own().filter(|c| c.pos == Position::Reserve).count();

        let mut home = String::new();
        for depth in 0..HOME_DEPTH as u8 {
            let taken = own().any(|c| c.pos == Position::Home { depth });
            home.push(if taken { '#' } else { '_' });
        }

        let moon: Vec<char> = own()
            .filter_map(|c| match c.pos {
                Position::Moon { field, .. } => Some(match field.required_roll() {
                    1 => '1',
                    3 => '3',
                    _ => '6',
                }),
                _ => None,
            })
            .collect();

        let captured = own()
            .filter(|c| matches!(c.pos, Position::Captured { .. }))
            .count();

        lines.push(format!(
            "Сторона {}: резерв {reserve} | дом [{home}] | луна {:?} | плен {captured}",
            side.letter(),
            moon,
        ));
    }
    lines.join("\n")
}

/// Полное текстовое представление состояния партии.
pub fn render(state: &GameState) -> String {
    format!(
        "Ход стороны: {}\n{}\n\n{}",
        state.to_move.letter(),
        perimeter(state),
        off_board(state),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::LOCAL_HOME_ENTRANCE;

    #[test]
    fn prison_cells_and_captive_render_distinctly() {
        use crate::board::LOCAL_PRISON_NEAR;
        let prison = Side::A.local_to_perimeter(LOCAL_PRISON_NEAR);
        let (pr, pc) = margin_coord(prison);

        // Пустая Тюрьма — глиф Prison.
        let empty = GameState::new(vec![Side::A, Side::C], Side::A);
        assert_eq!(board_glyphs(&empty)[pr][pc], Glyph::Prison('J'));

        // Пленённая фишка — глиф Captive со своей стороной.
        let mut jailed = empty.clone();
        jailed.checkers[0].pos = Position::Prison { cell: prison };
        assert_eq!(board_glyphs(&jailed)[pr][pc], Glyph::Captive(Side::A));
    }

    #[test]
    fn home_and_moon_checkers_appear_inside_the_square() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        state.checkers[0].pos = Position::Home { depth: 0 };
        state.checkers[1].pos = Position::Moon {
            side: Side::A,
            field: MoonField::One,
        };
        let grid = board_glyphs(&state);

        // Фишка в Доме — на первой клетке внутрь от входа в Дом.
        let (hr, hc) = inward_cell(Side::A.entry(), 1);
        assert_eq!(grid[hr][hc], Glyph::Checker(Side::A));
        // Фишка на Луне (поле «1») — на первой клетке внутрь от входа на Луну.
        let (mr, mc) = inward_cell(Side::A.local_to_perimeter(LOCAL_MOON), 1);
        assert_eq!(grid[mr][mc], Glyph::Checker(Side::A));

        // Пустые слоты Луны размечены глифом Луны с цифрой (поле «1» занято фишкой).
        let (r3, c3) = inward_cell(Side::A.local_to_perimeter(LOCAL_MOON), 2);
        assert_eq!(grid[r3][c3], Glyph::Moon('3'));
    }

    #[test]
    fn corners_map_to_square_corners() {
        let last = SIDE_LEN - 1;
        assert_eq!(cell_coord(0), (last, 0)); // нижне-левый
        assert_eq!(cell_coord(18), (last, last)); // нижне-правый
        assert_eq!(cell_coord(36), (0, last)); // верхне-правый
        assert_eq!(cell_coord(54), (0, 0)); // верхне-левый
    }

    #[test]
    fn render_contains_landmarks_and_turn() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        // Поставим фишку A на её вход в Дом — должна появиться метка `A1`.
        state.checkers[0].pos = Position::OnTrack { progress: 0 };
        let out = render(&state);
        assert!(out.contains("Ход стороны: A"));
        assert!(out.contains('+')); // углы
        assert!(out.contains('M')); // Луна
        assert!(out.contains('J')); // Тюрьма
        assert!(out.contains("A1")); // фишка A на дорожке
        assert!(out.contains("Сторона A"));
        // На входе в Дом действительно стоит наша фишка.
        let entry = Side::A.local_to_perimeter(LOCAL_HOME_ENTRANCE);
        assert_eq!(token(&state, entry), "A1");
    }
}
