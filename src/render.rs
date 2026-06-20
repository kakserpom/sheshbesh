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

use crate::board::{CellKind, HOME_DEPTH, PERIMETER, PerimeterIdx, SIDE_LEN, Side, cell_kind};
use crate::state::{GameState, Position};

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
