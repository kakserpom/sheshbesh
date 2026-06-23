use sheshbesh::board::LOCAL_PRISON_NEAR;
use sheshbesh::{Checker, Game, GameState, MoonField, Position, Side};

// --- Режим разработчика: демонстрационные состояния доски ---

/// Сценарий-демо: предельные/наглядные расстановки для проверки отрисовки.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Demo {
    Reserve,
    Moon(MoonField),
    Cell,
    Prison,
    PrisonStack,
    Captured,
    Homes,
}

/// Список сценариев и их подписи для панели разработчика.
pub(crate) const DEMOS: &[(Demo, &str)] = &[
    (Demo::Reserve, "Резерв (старт)"),
    (Demo::Moon(MoonField::One), "16 фишек на Луне — поле 1"),
    (Demo::Moon(MoonField::Three), "16 фишек на Луне — поле 3"),
    (Demo::Moon(MoonField::Six), "16 фишек на Луне — поле 6"),
    (Demo::Cell, "16 фишек на углу"),
    (Demo::Prison, "16 фишек в одной Тюрьме"),
    (Demo::PrisonStack, "Тюрьма: стопки одного цвета"),
    (Demo::Captured, "12 фишек в плену (макс)"),
    (Demo::Homes, "Дома заполнены"),
];

/// Состояние со всеми четырьмя сторонами; позиция каждой фишки задаётся
/// `place(side, k)`, где `k` — её индекс (0..4) среди фишек своей стороны.
pub(crate) fn demo_state(place: impl Fn(Side, usize) -> Position) -> GameState {
    let mut s = GameState::new(Side::ALL.to_vec(), Side::A);
    let mut seen: Vec<Side> = Vec::new();
    for ch in &mut s.checkers {
        let k = seen.iter().filter(|&&o| o == ch.owner).count();
        seen.push(ch.owner);
        ch.pos = place(ch.owner, k);
    }
    s
}

/// Строит состояние для выбранного сценария-демо.
pub(crate) fn demo_game(demo: Demo) -> GameState {
    match demo {
        Demo::Reserve => demo_state(|_, _| Position::Reserve),
        Demo::Moon(field) => demo_state(move |_, _| Position::Moon {
            side: Side::A,
            field,
        }),
        Demo::Cell => {
            // Все 16 на одном углу (углы и Тюрьма — единственные клетки с разными
            // фишками): у каждой стороны свой прогресс до этой абсолютной клетки.
            let corner = Side::A.start_corner();
            demo_state(move |side, _| Position::OnTrack {
                progress: side.progress_of(corner),
            })
        }
        Demo::Prison => {
            let cell = Side::A.local_to_perimeter(LOCAL_PRISON_NEAR);
            demo_state(move |_, _| Position::Prison { cell })
        }
        // Стопка на ВЫХОДЕ из Тюрьмы: освобождённые «зашёл-вышел» фишки СТОЯТ на клетке
        // Тюрьмы (как на углу). Все на одной клетке: A×4, B×3, C×2 — одноцветные
        // группируются со счётчиком, разные цвета разнесены наружу.
        Demo::PrisonStack => {
            let cell = Side::A.local_to_perimeter(LOCAL_PRISON_NEAR);
            demo_state(move |side, k| {
                let on_cell = Position::OnTrack {
                    progress: side.progress_of(cell),
                };
                match side {
                    Side::A => on_cell,
                    Side::B if k < 3 => on_cell,
                    Side::C if k < 2 => on_cell,
                    _ => Position::Reserve,
                }
            })
        }
        // Свою фишку в плен не берут: A держит максимум — по 4 от B/C/D (12), а её
        // собственные 4 фишки остаются в резерве.
        Demo::Captured => demo_state(|side, _| {
            if side == Side::A {
                Position::Reserve
            } else {
                Position::Captured { captor: Side::A }
            }
        }),
        Demo::Homes => demo_state(|_, k| Position::Home { depth: k as u8 }),
    }
}

/// Партия-заготовка для демо-анимации: фишка A на дорожке съедает фишку C впереди
/// (остальные — в Домах, чтобы не мешать). Бросок «3 и 1» делает ход однозначным.
pub(crate) fn demo_capture() -> Game {
    let target = Side::A.entry().advance(3);
    let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
    s.checkers.clear();
    s.checkers.push(Checker {
        owner: Side::A,
        pos: Position::OnTrack { progress: 0 },
    });
    for d in 0..3 {
        s.checkers.push(Checker {
            owner: Side::A,
            pos: Position::Home { depth: d },
        });
    }
    s.checkers.push(Checker {
        owner: Side::C,
        pos: Position::OnTrack {
            progress: Side::C.progress_of(target),
        },
    });
    for d in 0..3 {
        s.checkers.push(Checker {
            owner: Side::C,
            pos: Position::Home { depth: d },
        });
    }
    Game::new(s)
}
