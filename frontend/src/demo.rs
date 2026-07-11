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
    CornerTwo,
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
    Demo::CornerTwo,
];

/// Состояние со всеми четырьмя сторонами; позиция каждой фишки задаётся
/// `place(side, k)`, где `k` — её индекс (0..4) среди фишек своей стороны.
pub(crate) fn demo_state(place: impl Fn(Side, usize) -> Position) -> GameState {
    let mut s = GameState::new(Side::ALL.to_vec(), Side::A);
    let mut seen: Vec<Side> = Vec::new();
    for idx in 0..s.checkers_len() {
        let owner = s.checker(idx).owner;
        let k = seen.iter().filter(|&&o| o == owner).count();
        seen.push(owner);
        s.set_checker_pos(idx, place(owner, k));
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
        Demo::CornerTwo => {
            let corner = Side::A.entry().get() as u16; // progress 0, первый угол
            demo_state(|side, k| match (side, k) {
                (Side::A, 0) | (Side::A, 1) => Position::OnTrack { progress: corner },
                (Side::A, 2) => Position::OnTrack {
                    progress: corner + 1,
                },
                (Side::A, 3) => Position::OnTrack {
                    progress: corner + 2,
                },
                _ => Position::Reserve,
            })
        }
    }
}

/// Расстановка для интерактивной проверки хода у Дома.
pub(crate) fn home_entry_test() -> GameState {
    let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
    s.clear_checkers();
    for progress in [71u16, 69] {
        s.push_checker(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress },
        });
    }
    for depth in [2u8, 3] {
        s.push_checker(Checker {
            owner: Side::A,
            pos: Position::Home { depth },
        });
    }
    for _ in 0..4 {
        s.push_checker(Checker {
            owner: Side::C,
            pos: Position::Reserve,
        });
    }
    s
}

/// Расстановка для проверки захода на Луну: A на шаг от клетки Луны, бросок [1, 2].
pub(crate) fn moon_entry_test() -> GameState {
    let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
    s.clear_checkers();
    s.push_checker(Checker {
        owner: Side::A,
        pos: Position::OnTrack { progress: 64 },
    });
    for _ in 0..3 {
        s.push_checker(Checker {
            owner: Side::A,
            pos: Position::Reserve,
        });
    }
    for _ in 0..4 {
        s.push_checker(Checker {
            owner: Side::C,
            pos: Position::Reserve,
        });
    }
    s
}

/// Расстановка для проверки хода на угол со своей фишкой.
/// A0 на углу (progress 9), A1 за 6 от него (progress 3), бросок [6, 1].
pub(crate) fn corner_test() -> GameState {
    let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
    s.clear_checkers();
    s.push_checker(Checker {
        owner: Side::A,
        pos: Position::OnTrack { progress: 9 },
    });
    s.push_checker(Checker {
        owner: Side::A,
        pos: Position::OnTrack { progress: 3 },
    });
    for _ in 0..2 {
        s.push_checker(Checker {
            owner: Side::A,
            pos: Position::Reserve,
        });
    }
    for _ in 0..4 {
        s.push_checker(Checker {
            owner: Side::C,
            pos: Position::Reserve,
        });
    }
    s
}

/// Партия-заготовка для проверки выкупа: A может выкупить фишку у C (шестёркой),
/// затем доиграть тройкой. У C ровно одна фишка на дорожке со свободным ходом на 6.
/// Все OnTrack-позиции лежат на plain-клетках (не Луна, не Тюрьма).
pub(crate) fn ransom_test() -> GameState {
    let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
    s.clear_checkers();
    // A: 3 фишки на plain-клетках (не corner/moon/prison), 1 в плену у C.
    for progress in [70u16, 69, 58] {
        s.push_checker(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress },
        });
    }
    s.push_checker(Checker {
        owner: Side::A,
        pos: Position::Captured { captor: Side::C },
    });
    // C: 1 фишка на plain-клетке с ходом на +6, 3 в Доме.
    // progress 6 → abs 51 (Side C local 15, plain); +6 → progress 12, путь чист.
    s.push_checker(Checker {
        owner: Side::C,
        pos: Position::OnTrack { progress: 6 },
    });
    for depth in [0u8, 1, 2] {
        s.push_checker(Checker {
            owner: Side::C,
            pos: Position::Home { depth },
        });
    }
    s
}

/// Партия-заготовка для демо-анимации.
pub(crate) fn demo_capture() -> Game {
    let target = Side::A.entry().advance(3);
    let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
    s.clear_checkers();
    s.push_checker(Checker {
        owner: Side::A,
        pos: Position::OnTrack { progress: 0 },
    });
    for d in 0..3 {
        s.push_checker(Checker {
            owner: Side::A,
            pos: Position::Home { depth: d },
        });
    }
    s.push_checker(Checker {
        owner: Side::C,
        pos: Position::OnTrack {
            progress: Side::C.progress_of(target),
        },
    });
    for d in 0..3 {
        s.push_checker(Checker {
            owner: Side::C,
            pos: Position::Home { depth: d },
        });
    }
    Game::new(s)
}
