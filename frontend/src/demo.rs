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

/// Список сценариев (без подписей — локализация в app.rs через i18n).
pub(crate) const DEMOS: &[Demo] = &[
    Demo::Reserve,
    Demo::Moon(MoonField::One),
    Demo::Moon(MoonField::Three),
    Demo::Moon(MoonField::Six),
    Demo::Cell,
    Demo::Prison,
    Demo::PrisonStack,
    Demo::Captured,
    Demo::Homes,
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
            let corner = Side::A.start_corner();
            demo_state(move |side, _| Position::OnTrack {
                progress: side.progress_of(corner),
            })
        }
        Demo::Prison => {
            let cell = Side::A.local_to_perimeter(LOCAL_PRISON_NEAR);
            demo_state(move |_, _| Position::Prison { cell })
        }
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

/// Расстановка для интерактивной проверки хода у Дома.
pub(crate) fn home_entry_test() -> GameState {
    let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
    s.checkers.clear();
    for progress in [71u16, 69] {
        s.checkers.push(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress },
        });
    }
    for depth in [2u8, 3] {
        s.checkers.push(Checker {
            owner: Side::A,
            pos: Position::Home { depth },
        });
    }
    for _ in 0..4 {
        s.checkers.push(Checker {
            owner: Side::C,
            pos: Position::Reserve,
        });
    }
    s
}

/// Партия-заготовка для демо-анимации.
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
