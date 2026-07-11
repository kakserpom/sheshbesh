//! Геометрия доски: стороны, периметр, классификация особых клеток.
//!
//! Доска — квадрат. У каждой стороны 19 клеток, включая угловые на концах.
//! Углы общие для двух смежных сторон, поэтому периметр = 4×19 − 4 = 72 клетки.
//! Фишки обходят периметр против часовой стрелки.

/// Число сторон (и потенциальных игроков/Домов).
pub const SIDES: usize = 4;
/// Клеток на одной стороне, включая оба угла.
pub const SIDE_LEN: usize = 19;
/// Длина периметра: 4×19 минус 4 общих угла.
pub const PERIMETER: usize = SIDES * (SIDE_LEN - 1); // 72
/// Глубина Дома (клеток внутрь квадрата).
pub const HOME_DEPTH: usize = 4;
/// Фишек у каждого игрока.
pub const CHECKERS_PER_PLAYER: usize = 4;

// --- Локальные индексы вдоль стороны (0..=18; 0 и 18 — углы) ---
/// Угол, с которого фишка входит на сторону (двигаясь против часовой стрелки).
pub const LOCAL_ENTRY_CORNER: u8 = 0;
/// Луна (вход во внутреннюю дорожку).
pub const LOCAL_MOON: u8 = 2;
/// Ближняя к входу Тюрьма (4-я клетка от входного угла).
pub const LOCAL_PRISON_NEAR: u8 = 4;
/// Вход в Дом и точка ввода фишки в игру (середина стороны).
pub const LOCAL_HOME_ENTRANCE: u8 = 9;
/// Дальняя Тюрьма (4-я клетка от дальнего угла).
pub const LOCAL_PRISON_FAR: u8 = 14;
/// Клетка выхода с Луны (симметрична входу относительно середины).
pub const LOCAL_MOON_EXIT: u8 = 16;
/// Дальний угол стороны (он же `LOCAL_ENTRY_CORNER` следующей стороны).
pub const LOCAL_EXIT_CORNER: u8 = 18;

/// Сторона квадрата. Стороны перечислены против часовой стрелки: `A → B → C → D`.
/// Каждая сторона владеет своим Домом; в игре вдвоём используются противоположные.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum Side {
    A,
    B,
    C,
    D,
}

impl Side {
    /// Все стороны в порядке против часовой стрелки.
    pub const ALL: [Side; SIDES] = [Side::A, Side::B, Side::C, Side::D];

    /// Порядковый номер стороны (0..=3).
    pub fn index(self) -> usize {
        match self {
            Side::A => 0,
            Side::B => 1,
            Side::C => 2,
            Side::D => 3,
        }
    }

    /// Сторона по порядковому номеру (по модулю 4).
    pub fn from_index(i: usize) -> Side {
        Side::ALL[i % SIDES]
    }

    /// Буква-обозначение стороны (`A`/`B`/`C`/`D`) — для вывода.
    pub fn letter(self) -> char {
        ['A', 'B', 'C', 'D'][self.index()]
    }

    /// Противоположная сторона (для игры вдвоём).
    pub fn opposite(self) -> Side {
        Side::from_index(self.index() + 2)
    }

    /// Абсолютный индекс угла, с которого начинается эта сторона.
    pub fn start_corner(self) -> PerimeterIdx {
        PerimeterIdx::new(self.index() * (SIDE_LEN - 1))
    }

    /// Абсолютный индекс периметра по локальному индексу стороны (0..=18).
    ///
    /// Локальный индекс 18 совпадает с углом следующей стороны (углы общие).
    pub fn local_to_perimeter(self, local: u8) -> PerimeterIdx {
        debug_assert!((local as usize) < SIDE_LEN);
        PerimeterIdx::new(self.start_corner().get() + local as usize)
    }

    /// Точка ввода в игру и захода в Дом для владельца этой стороны
    /// (середина стороны, `LOCAL_HOME_ENTRANCE`).
    pub fn entry(self) -> PerimeterIdx {
        self.local_to_perimeter(LOCAL_HOME_ENTRANCE)
    }

    /// Прогресс (число шагов против часовой стрелки от точки ввода владельца)
    /// для абсолютной клетки `idx`. Обратное к `OnTrack { progress }` → клетка.
    pub fn progress_of(self, idx: PerimeterIdx) -> u16 {
        ((idx.get() + PERIMETER - self.entry().get()) % PERIMETER) as u16
    }
}

/// Индекс клетки периметра (0..71), с кольцевой арифметикой против часовой стрелки.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct PerimeterIdx(usize);

impl PerimeterIdx {
    /// Создаёт индекс, приводя его по модулю длины периметра.
    pub fn new(value: usize) -> Self {
        PerimeterIdx(value % PERIMETER)
    }

    /// Числовое значение индекса (0..71).
    pub fn get(self) -> usize {
        self.0
    }

    /// Сдвиг на `steps` клеток против часовой стрелки (с заворотом).
    pub fn advance(self, steps: usize) -> Self {
        PerimeterIdx::new(self.0 + steps)
    }

    /// Сторона, которой принадлежит клетка. Углы относятся к стороне,
    /// которая на них начинается (`start_corner`).
    pub fn side(self) -> Side {
        Side::from_index(self.0 / (SIDE_LEN - 1))
    }

    /// Локальный индекс вдоль стороны (0..=17; угол — 0).
    pub fn local(self) -> u8 {
        (self.0 % (SIDE_LEN - 1)) as u8
    }
}

/// Геометрический тип клетки периметра (не зависит от игрока).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum CellKind {
    /// Обычная клетка.
    Plain,
    /// Угол: общий для двух сторон, на нём фишки не едят.
    Corner,
    /// Луна: попав сюда, фишка улетает на внутреннюю дорожку.
    Moon,
    /// Тюрьма: изолирует фишку (выход — выброс «4»).
    Prison,
    /// Вход в Дом владельца стороны (для остальных — обычная клетка).
    HomeEntrance,
}

/// Классифицирует клетку периметра по её геометрии.
pub fn cell_kind(idx: PerimeterIdx) -> CellKind {
    // Углы лежат на стыках сторон: абсолютный индекс кратен длине стороны.
    if idx.get().is_multiple_of(SIDE_LEN - 1) {
        return CellKind::Corner;
    }
    match idx.local() {
        LOCAL_MOON => CellKind::Moon,
        LOCAL_PRISON_NEAR | LOCAL_PRISON_FAR => CellKind::Prison,
        LOCAL_HOME_ENTRANCE => CellKind::HomeEntrance,
        _ => CellKind::Plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perimeter_length() {
        assert_eq!(PERIMETER, 72);
    }

    #[test]
    fn corners_are_shared_and_evenly_spaced() {
        for side in Side::ALL {
            let corner = side.start_corner();
            assert_eq!(cell_kind(corner), CellKind::Corner);
        }
        assert_eq!(Side::A.start_corner().get(), 0);
        assert_eq!(Side::B.start_corner().get(), 18);
        assert_eq!(Side::C.start_corner().get(), 36);
        assert_eq!(Side::D.start_corner().get(), 54);
        // Дальний угол стороны A совпадает с началом стороны B.
        assert_eq!(
            Side::A.local_to_perimeter(LOCAL_EXIT_CORNER),
            Side::B.start_corner()
        );
    }

    #[test]
    fn special_cells_on_each_side() {
        for side in Side::ALL {
            assert_eq!(
                cell_kind(side.local_to_perimeter(LOCAL_MOON)),
                CellKind::Moon
            );
            assert_eq!(
                cell_kind(side.local_to_perimeter(LOCAL_PRISON_NEAR)),
                CellKind::Prison
            );
            assert_eq!(
                cell_kind(side.local_to_perimeter(LOCAL_PRISON_FAR)),
                CellKind::Prison
            );
            assert_eq!(
                cell_kind(side.local_to_perimeter(LOCAL_HOME_ENTRANCE)),
                CellKind::HomeEntrance
            );
            // Выход Луны — обычная клетка, симметричная входу относительно середины.
            assert_eq!(
                cell_kind(side.local_to_perimeter(LOCAL_MOON_EXIT)),
                CellKind::Plain
            );
            assert_eq!(LOCAL_MOON + LOCAL_MOON_EXIT, 2 * LOCAL_HOME_ENTRANCE);
        }
    }

    #[test]
    fn opposite_sides() {
        assert_eq!(Side::A.opposite(), Side::C);
        assert_eq!(Side::B.opposite(), Side::D);
        assert_eq!(Side::C.opposite(), Side::A);
    }

    #[test]
    fn advance_wraps_around() {
        assert_eq!(PerimeterIdx::new(70).advance(5).get(), 3);
    }
}
