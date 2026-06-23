use crate::util::{Frame, HOLD_ROLL_MS, ROLL_ANIM_MS};
use sheshbesh::{
    Checker, DiceRoll, Die, Game, GameState, Move, MoveKind, Position, Side, legal_turns,
};

// --- Обучение (туториал) ---

/// Один шаг туториала = граница хода: позиция `before` в начале шага и заранее
/// заданный (без случайности) бросок `roll` для хода из этой позиции в следующую.
/// Ход вычисляет сам движок (`legal_turns`), поэтому он гарантированно легален и
/// соблюдает правило максимального хода. `None` в `roll` — шаг-пояснение без хода.
pub(crate) struct Lesson {
    pub(crate) title: &'static str,
    pub(crate) text: &'static str,
    /// Позиция в начале хода игрока (сторона A).
    pub(crate) before: GameState,
    /// Бросок этого хода (`None` — шаг-пояснение без хода).
    pub(crate) roll: Option<DiceRoll>,
    /// Ход игрока (под-ходы по костям), посчитанный движком.
    pub(crate) moves: Vec<Move>,
    /// Ход соперника (C), который автоматически проигрывается после хода игрока,
    /// прежде чем дойти до следующего шага — чтобы стороны ходили по очереди.
    pub(crate) opp: Option<OppTurn>,
    /// Ход целиком проигрывается через `commit_turn`/`commit_frames` (с выкупом и
    /// обязательным ответным ходом захватчика), а не покликовыми под-ходами.
    pub(crate) commit: bool,
    /// Позиция `before` — скачок относительно конца предыдущего шага (расстановка
    /// задана вручную, а не получена ходом). При переходе к такому шагу доска
    /// гасится/проявляется (fade), чтобы фишки не «перелетали» через всю доску.
    pub(crate) teleport: bool,
}

/// Авто-ход соперника между ходами игрока.
pub(crate) struct OppTurn {
    pub(crate) roll: DiceRoll,
    pub(crate) moves: Vec<Move>,
}

/// Пара значений → бросок двух костей.
pub(crate) fn dr(a: u8, b: u8) -> DiceRoll {
    DiceRoll::new(Die::new(a).expect("1..6"), Die::new(b).expect("1..6"))
}

/// Туториал как **непрерывная партия** A против C по заранее заданным броскам: одна
/// фишка A проходит путь (ввод→Тюрьма→Луна→съедание), а между её ходами соперник C
/// двигает свою фишку-«мувера» (`checkers[5]`), не мешая пути; фишка-мишень C
/// (`checkers[4]`) стоит до съедания. Все ходы считает движок (`legal_turns`),
/// поэтому они легальны и соблюдают правило максимального хода.
pub(crate) fn lessons() -> Vec<Lesson> {
    use Side::{A, C};
    // Стартовая позиция непрерывной партии.
    let mut s = GameState::new(vec![A, C], A);
    s.checkers.clear();
    s.checkers.push(Checker {
        owner: A,
        pos: Position::Reserve,
    }); // A[0] — демо-фишка
    for d in 1..4u8 {
        s.checkers.push(Checker {
            owner: A,
            pos: Position::Home { depth: d },
        }); // A[1..3] инертны
    }
    s.checkers.push(Checker {
        owner: C,
        pos: Position::OnTrack { progress: 64 },
    }); // C[4] мишень
    s.checkers.push(Checker {
        owner: C,
        pos: Position::OnTrack { progress: 5 },
    }); // C[5] мувер
    for d in 1..3u8 {
        s.checkers.push(Checker {
            owner: C,
            pos: Position::Home { depth: d },
        }); // C[6,7]
    }

    // Ход игрока (A) — единственный легальный (движок форсирует путь).
    let a_turn =
        |st: &GameState, r: DiceRoll| legal_turns(st, r).into_iter().next().unwrap_or_default();
    // Ход соперника (C) — максимальный, двигающий ТОЛЬКО мувера C[5], иначе любой.
    let c_turn = |st: &GameState, r: DiceRoll| {
        let turns = legal_turns(st, r);
        turns
            .iter()
            .find(|t| !t.is_empty() && t.iter().all(|m| m.checker == 5))
            .or_else(|| turns.first())
            .cloned()
            .unwrap_or_default()
    };

    // Сценарий: (бросок A, бросок C после, заголовок, текст). Последний шаг — без C.
    let script: [(DiceRoll, Option<DiceRoll>, &str, &str); 6] = [
        (
            dr(6, 5),
            Some(dr(3, 2)),
            "Цель и ввод",
            "У каждого 4 фишки в резерве; цель — обойти доску против часовой и собрать все в своём Доме. \
          Выпало 6 и 5: по 6 фишка входит на точку входа, затем оставшейся 5 доходит до Тюрьмы. \
          Нажмите фишку, затем клетку (по одной кости), или ▶.",
        ),
        (
            dr(4, 6),
            Some(dr(4, 1)),
            "Тюрьма и Луна",
            "Тюрьма держит фишку: выйти можно только выбросив 4 — она выпала. Затем оставшейся 6 фишка \
          идёт дальше и попадает на Луну. Сделайте оба под-хода.",
        ),
        (
            dr(1, 2),
            Some(dr(3, 1)),
            "Луна",
            "Луна — короткий безопасный путь по полям 1·3·6 (съесть нельзя). Выпало 1 — переход с поля «1» \
          на «3». Вторая кость (2) не играется: на Луне нужно точное значение. Сделайте ход.",
        ),
        (
            dr(3, 2),
            Some(dr(2, 1)),
            "Дорожка Луны",
            "Выпало 3 — переход с поля «3» на «6». Осталось выбросить 6, чтобы сойти с Луны.",
        ),
        (
            dr(6, 1),
            Some(dr(4, 2)),
            "Сход с Луны",
            "По 6 фишка сходит с Луны на доску, заметно ближе к Дому. Впереди — фишка соперника.",
        ),
        (
            // 2-4 (а не 2-1): после съедания фишка встаёт на обычную клетку, а не на
            // вход Луны — иначе «улетела» бы на Луну во второй раз.
            dr(2, 4),
            None,
            "Съедание",
            "Встав на клетку с фишкой соперника, фишка съедает её — пленная уходит к нашему Дому \
          (с красной обводкой). На углах, Луне и в Тюрьме не едят.",
        ),
    ];

    let mut out = Vec::new();
    let mut g = Game::new(s);
    for (a_roll, c_roll, title, text) in script {
        let before = g.state.clone();
        let moves = a_turn(&g.state, a_roll);
        g.commit_turn(a_roll, moves.clone(), |_, _, _| 0);
        let opp = c_roll.map(|cr| {
            let cm = c_turn(&g.state, cr);
            g.commit_turn(cr, cm.clone(), |_, _, _| 0);
            OppTurn {
                roll: cr,
                moves: cm,
            }
        });
        out.push(Lesson {
            title,
            text,
            before,
            roll: Some(a_roll),
            moves,
            opp,
            commit: false,
            teleport: false,
        });
    }

    // Шаг выкупа — ПРОДОЛЖЕНИЕ той же партии (без перескока на чужую расстановку):
    // мы только что съели фишку C (она у нас в плену), ход за C. Чтобы показать
    // обязательный ответный ход захватчика на 6, выводим нашу фишку с чужой Луны на
    // дорожку — единственную, которой будет чем сходить на 6 (домашние фишки не могут).
    let ransom_before = {
        let mut s = g.state.clone(); // после съедания: C-фишка в плену у A, ход C
        // Ставим у Дома: ход на 6 доведёт фишку до клетки перед Домом, а на финальном
        // шаге она зайдёт в Дом настоящим ходом (а не «перепрыгнет»).
        s.checkers[0].pos = Position::OnTrack { progress: 64 };
        // Мувер C уводим с пути нашей фишки (её обязательный ход на 6 идёт по
        // A-прогрессу 64→70): иначе C-фишка оказалась бы на этом отрезке и наша
        // «переступила» бы её при выкупном ходе. C-прогресс 50 = A-прогресс 14 — в стороне.
        s.checkers[5].pos = Position::OnTrack { progress: 50 };
        s
    };
    let ransom_roll = dr(6, 2);
    let ransom_moves = legal_turns(&ransom_before, ransom_roll)
        .into_iter()
        .find(|t| t.iter().any(|m| m.kind == MoveKind::Ransom))
        .unwrap_or_default();
    out.push(Lesson {
        title: "Плен и выкуп",
        text: "Чтобы вернуть пленную фишку, её владелец выбрасывает 6 — она возвращается в резерв \
               (выкуп). Соперник тут же выкупает свою фишку (она улетает к нему в резерв). \
               Тогда захватчик обязан сразу сходить на 6 клеток: выберите, какой нашей фишкой — \
               нажмите фишку, затем подсвеченную клетку.",
        before: ransom_before.clone(),
        roll: Some(ransom_roll),
        moves: ransom_moves.clone(),
        opp: None,
        commit: true,
        teleport: true, // фишку переставили к Дому — переход к шагу гасим/проявляем
    });

    // Состояние после выкупа и обязательного хода на 6 — основа финального шага, чтобы
    // и переход к «Дому» двигал лишь нашу фишку, без перескоков расстановки.
    let after_ransom = {
        let mut rg = Game::new(ransom_before);
        rg.commit_turn(ransom_roll, ransom_moves, |_, _, _| 0);
        rg.state
    };
    // Финальный шаг — НАСТОЯЩИЙ ход: наша фишка (уже у Дома после хода на 6) заходит
    // в Дом по броску, а не «перепрыгивает». Остальные три фишки A уже в Доме, поэтому
    // этим ходом партия выигрывается.
    let home_roll = dr(2, 1);
    let home_moves = a_turn(&after_ransom, home_roll);
    out.push(Lesson {
        title: "Дом и победа",
        text: "Дойдя до Дома, фишка заходит ровно по броску — по одной в клетку; перепрыгивать занятые \
               клетки Дома нельзя (но можно идти глубже). Чужие фишки в Дом не пускают. Заведите фишку \
               в Дом этим ходом — все 4 фишки дома, и вы выиграли!",
        before: after_ransom,
        roll: Some(home_roll),
        moves: home_moves,
        opp: None,
        commit: false,
        teleport: false,
    });
    out
}

/// Кадры броска кости БЕЗ движения фишки: кубики кувыркаются (3D), затем застывают.
/// Бросок происходит ДО хода игрока (как в реальной игре).
pub(crate) fn roll_only_frames(state: &GameState, roll: DiceRoll) -> Vec<Frame> {
    vec![
        Frame {
            state: state.clone(),
            roll: Some(roll),
            hold: ROLL_ANIM_MS,
            rolling: true,
            note: None,
            pts: Vec::new(),
            fade: false,
        },
        Frame {
            state: state.clone(),
            roll: Some(roll),
            hold: HOLD_ROLL_MS,
            rolling: false,
            note: None,
            pts: Vec::new(),
            fade: false,
        },
    ]
}
