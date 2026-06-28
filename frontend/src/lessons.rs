use crate::i18n::*;
use crate::util::{Frame, HOLD_ROLL_MS, ROLL_ANIM_MS};
use leptos_i18n::I18nContext;
use sheshbesh::{
    Checker, DiceRoll, Die, Game, GameState, Move, MoveKind, Position, Side, legal_turns,
};

// --- Обучение (туториал) ---

/// Один шаг туториала = граница хода.
pub(crate) struct Lesson {
    pub(crate) title: String,
    pub(crate) text: String,
    /// Позиция в начале хода игрока (сторона A).
    pub(crate) before: GameState,
    /// Бросок этого хода (`None` — шаг-пояснение без хода).
    pub(crate) roll: Option<DiceRoll>,
    /// Ход игрока (под-ходы по костям), посчитанный движком.
    pub(crate) moves: Vec<Move>,
    /// Ход соперника (C), который автоматически проигрывается после хода игрока.
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
pub(crate) fn lessons(i18n: I18nContext<Locale>) -> Vec<Lesson> {
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

    // Сценарий: (бросок A, бросок C после, заголовок, текст).
    let script: [(DiceRoll, Option<DiceRoll>, String, String); 6] = [
        (
            dr(6, 5),
            Some(dr(3, 2)),
            tu_string!(i18n, lesson_title_1).to_string(),
            tu_string!(i18n, lesson_text_1).to_string(),
        ),
        (
            dr(4, 6),
            Some(dr(4, 1)),
            tu_string!(i18n, lesson_title_2).to_string(),
            tu_string!(i18n, lesson_text_2).to_string(),
        ),
        (
            dr(1, 2),
            Some(dr(3, 1)),
            tu_string!(i18n, lesson_title_3).to_string(),
            tu_string!(i18n, lesson_text_3).to_string(),
        ),
        (
            dr(3, 2),
            Some(dr(2, 1)),
            tu_string!(i18n, lesson_title_4).to_string(),
            tu_string!(i18n, lesson_text_4).to_string(),
        ),
        (
            dr(6, 1),
            Some(dr(4, 2)),
            tu_string!(i18n, lesson_title_5).to_string(),
            tu_string!(i18n, lesson_text_5).to_string(),
        ),
        (
            dr(2, 4),
            None,
            tu_string!(i18n, lesson_title_6).to_string(),
            tu_string!(i18n, lesson_text_6).to_string(),
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

    // Шаг выкупа
    let ransom_before = {
        let mut s = g.state.clone();
        s.checkers[0].pos = Position::OnTrack { progress: 64 };
        s.checkers[5].pos = Position::OnTrack { progress: 50 };
        s
    };
    let ransom_roll = dr(6, 2);
    let ransom_moves = legal_turns(&ransom_before, ransom_roll)
        .into_iter()
        .find(|t| t.iter().any(|m| m.kind == MoveKind::Ransom))
        .unwrap_or_default();
    out.push(Lesson {
        title: tu_string!(i18n, lesson_title_7).to_string(),
        text: tu_string!(i18n, lesson_text_7).to_string(),
        before: ransom_before.clone(),
        roll: Some(ransom_roll),
        moves: ransom_moves.clone(),
        opp: None,
        commit: true,
        teleport: true,
    });

    // Состояние после выкупа и обязательного хода на 6
    let after_ransom = {
        let mut rg = Game::new(ransom_before);
        rg.commit_turn(ransom_roll, ransom_moves, |_, _, _| 0);
        rg.state
    };
    // Финальный шаг
    let home_roll = dr(2, 1);
    let home_moves = a_turn(&after_ransom, home_roll);
    out.push(Lesson {
        title: tu_string!(i18n, lesson_title_8).to_string(),
        text: tu_string!(i18n, lesson_text_8).to_string(),
        before: after_ransom,
        roll: Some(home_roll),
        moves: home_moves,
        opp: None,
        commit: false,
        teleport: false,
    });
    out
}

/// Кадры броска кости БЕЗ движения фишки.
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
            sound: None,
        },
        Frame {
            state: state.clone(),
            roll: Some(roll),
            hold: HOLD_ROLL_MS,
            rolling: false,
            note: None,
            pts: Vec::new(),
            fade: false,
            sound: None,
        },
    ]
}
