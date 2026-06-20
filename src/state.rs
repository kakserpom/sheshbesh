//! Состояние игры: фишки, их положение и общее состояние партии.

use crate::board::{CHECKERS_PER_PLAYER, LOCAL_HOME_ENTRANCE, PerimeterIdx, Side};

/// Поле внутренней дорожки Луны. Поля проходятся по порядку,
/// каждое продвигает только при совпадающем значении кости.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MoonField {
    /// Вход на Луну; для продвижения нужен выброс «1».
    One,
    /// Нужен выброс «3».
    Three,
    /// Нужен выброс «6»; затем фишка выходит на клетку выхода Луны.
    Six,
}

impl MoonField {
    /// Значение кости, которое продвигает фишку с этого поля.
    pub fn required_roll(self) -> u8 {
        match self {
            MoonField::One => 1,
            MoonField::Three => 3,
            MoonField::Six => 6,
        }
    }

    /// Следующее поле Луны; `None` означает выход с Луны на периметр.
    pub fn next(self) -> Option<MoonField> {
        match self {
            MoonField::One => Some(MoonField::Three),
            MoonField::Three => Some(MoonField::Six),
            MoonField::Six => None,
        }
    }
}

/// Положение одной фишки.
///
/// Положение на периметре хранится как `progress` — число шагов против часовой
/// стрелки от точки ввода владельца. Это однозначно кодирует и клетку, и пройденный
/// круг: при `progress == PERIMETER` фишка заходит в Дом (`Home { depth: 0 }`).
///
/// Точка ввода — клетка входа в Дом владельца (`LOCAL_HOME_ENTRANCE`).
/// ПРЕДПОЛОЖЕНИЕ (требует подтверждения): фишка вводится в игру именно здесь и,
/// обойдя круг, отсюда же заходит в Дом.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    /// В Доме, ещё не введена в игру (нужен выброс «6»). Сюда же попадает
    /// выкупленная из плена фишка перед повторным вводом.
    Reserve,
    /// На периметре: `progress` шагов (0..PERIMETER) от точки ввода владельца.
    OnTrack { progress: u16 },
    /// На внутренней дорожке Луны указанной стороны.
    Moon { side: Side, field: MoonField },
    /// В Тюрьме на клетке периметра (выход — выброс «4»).
    Prison { cell: PerimeterIdx },
    /// Внутри своего Дома, глубина 0..`HOME_DEPTH`.
    Home { depth: u8 },
    /// Съедена и удерживается соперником `captor` до выкупа.
    Captured { captor: Side },
}

impl Position {
    /// Абсолютный индекс клетки периметра, если фишка находится на периметре
    /// (на дорожке или в Тюрьме). Для Луны/Дома/резерва/плена — `None`.
    pub fn perimeter_cell(self, owner: Side) -> Option<PerimeterIdx> {
        match self {
            Position::OnTrack { progress } => {
                let entry = owner.local_to_perimeter(LOCAL_HOME_ENTRANCE);
                Some(entry.advance(progress as usize))
            }
            Position::Prison { cell } => Some(cell),
            _ => None,
        }
    }
}

/// Фишка: владелец (его сторона) и текущее положение.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Checker {
    pub owner: Side,
    pub pos: Position,
}

/// Полное состояние партии.
#[derive(Clone, Debug)]
pub struct GameState {
    /// Стороны, участвующие в партии (2 — противоположные, либо 4).
    pub active: Vec<Side>,
    /// Все фишки всех игроков.
    pub checkers: Vec<Checker>,
    /// Чей ход.
    pub to_move: Side,
}

impl GameState {
    /// Начальная расстановка: у каждой активной стороны `CHECKERS_PER_PLAYER`
    /// фишек в резерве (в Доме, ждут ввода выбросом «6»). Первый ход — за `first`.
    ///
    /// # Panics
    /// Если `active` пуст или `first` не входит в `active`.
    pub fn new(active: Vec<Side>, first: Side) -> GameState {
        assert!(!active.is_empty(), "нужна хотя бы одна сторона");
        assert!(
            active.contains(&first),
            "ходящая сторона должна быть активной"
        );

        let mut checkers = Vec::with_capacity(active.len() * CHECKERS_PER_PLAYER);
        for &side in &active {
            for _ in 0..CHECKERS_PER_PLAYER {
                checkers.push(Checker {
                    owner: side,
                    pos: Position::Reserve,
                });
            }
        }

        GameState {
            active,
            checkers,
            to_move: first,
        }
    }

    /// Фишки, стоящие на указанной клетке периметра (на дорожке или в Тюрьме).
    pub fn checkers_on(&self, cell: PerimeterIdx) -> impl Iterator<Item = &Checker> {
        self.checkers
            .iter()
            .filter(move |c| c.pos.perimeter_cell(c.owner) == Some(cell))
    }

    /// Достигла ли сторона цели: все её фишки в Доме.
    pub fn has_won(&self, side: Side) -> bool {
        self.checkers
            .iter()
            .filter(|c| c.owner == side)
            .all(|c| matches!(c.pos, Position::Home { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::PERIMETER;

    #[test]
    fn initial_setup_two_players() {
        let state = GameState::new(vec![Side::A, Side::C], Side::A);
        assert_eq!(state.checkers.len(), 2 * CHECKERS_PER_PLAYER);
        assert!(state.checkers.iter().all(|c| c.pos == Position::Reserve));
        assert_eq!(state.to_move, Side::A);
    }

    #[test]
    fn last_track_cell_is_one_before_entry() {
        // progress = PERIMETER - 1 — последняя клетка круга; абсолютно это клетка,
        // соседняя с точкой ввода (со стороны «против часовой»).
        let pos = Position::OnTrack {
            progress: (PERIMETER - 1) as u16,
        };
        let entry = Side::A.local_to_perimeter(LOCAL_HOME_ENTRANCE);
        assert_eq!(
            pos.perimeter_cell(Side::A),
            Some(entry.advance(PERIMETER - 1))
        );
    }

    #[test]
    fn progress_maps_to_entry_cell() {
        let entry = Side::A.local_to_perimeter(LOCAL_HOME_ENTRANCE);
        let pos = Position::OnTrack { progress: 0 };
        assert_eq!(pos.perimeter_cell(Side::A), Some(entry));
    }

    #[test]
    fn checkers_on_cell_finds_pieces() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        state.checkers[0].pos = Position::OnTrack { progress: 0 };
        let entry = Side::A.local_to_perimeter(LOCAL_HOME_ENTRANCE);
        assert_eq!(state.checkers_on(entry).count(), 1);
    }
}
