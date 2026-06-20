//! Интерактивный TUI на ratatui: доска слева, статус и панель справа.
//!
//! Партия проигрывается автоматически с заданным темпом; управление с клавиатуры:
//! `space` — пауза/продолжить, `s` — один ход (в т.ч. на паузе), `q`/`Esc` — выход.
//! Требует настоящего терминала (альт-экран); при выводе в пайп используйте
//! ANSI-анимацию из модуля [`crate::tui`].

use std::io;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{DefaultTerminal, Frame, init, restore};

use crate::board::{HOME_DEPTH, PERIMETER, PerimeterIdx, SIDE_LEN, Side};
use crate::dice::DiceRoll;
use crate::moves::{Move, MoveKind, apply, legal_turns};
use crate::render::{cell_coord, landmark, owners_on};
use crate::state::{GameState, Position};
use crate::turn::{Agent, DiceSource, Game, RandomDice, TurnOutcome};

/// Цвет стороны на доске.
fn side_color(side: Side) -> Color {
    match side {
        Side::A => Color::LightCyan,
        Side::B => Color::LightGreen,
        Side::C => Color::LightMagenta,
        Side::D => Color::LightYellow,
    }
}

/// Спан с буквой стороны в её цвете.
fn side_span(side: Side) -> Span<'static> {
    Span::styled(
        side.letter().to_string(),
        Style::default().fg(side_color(side)),
    )
}

/// Ширина полей вокруг квадрата: лишние фишки на клетке «дописываются» наружу,
/// до `MARGIN` штук за край (дальше — `+`).
const MARGIN: usize = 3;

/// Цветной 1-символьный маркер фишки стороны.
fn checker_marker(side: Side) -> Span<'static> {
    Span::styled(
        side.letter().to_string(),
        Style::default().fg(side_color(side)),
    )
}

/// Направление «наружу» от клетки на краю квадрата (углы — по вертикали).
fn outward(r: usize, c: usize) -> (isize, isize) {
    if r == 0 {
        (-1, 0) // верхний край — вверх
    } else if r == SIDE_LEN - 1 {
        (1, 0) // нижний край — вниз
    } else if c == 0 {
        (0, -1) // левый край — влево
    } else {
        (0, 1) // правый край — вправо
    }
}

/// Доска со внешними полями: каждая клетка — 1 символ (фишка или landmark),
/// лишние фишки клетки «дописываются» наружу. Клетки `from`/`to` подсвечиваются
/// (откуда — синим, куда — зелёным).
fn board_lines_hl(
    state: &GameState,
    from: Option<PerimeterIdx>,
    to: Option<PerimeterIdx>,
) -> Vec<Line<'static>> {
    let n = SIDE_LEN + 2 * MARGIN;
    let mut grid: Vec<Vec<Span>> = vec![vec![Span::raw(" "); n]; n];
    let shift = |base: usize, d: isize, k: isize| (base as isize + d * k) as usize;

    for abs in 0..PERIMETER {
        let (r, c) = cell_coord(abs);
        let p = PerimeterIdx::new(abs);
        let owners = owners_on(state, p);
        let (pr, pc) = (r + MARGIN, c + MARGIN);

        // Базовая клетка: первая фишка либо landmark; с подсветкой хода.
        let mut base = match owners.first() {
            Some(&s) => checker_marker(s),
            None => Span::styled(
                landmark(p).to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        };
        if Some(p) == to {
            base.style = base.style.bg(Color::Green);
        } else if Some(p) == from {
            base.style = base.style.bg(Color::Blue);
        }
        grid[pr][pc] = base;

        // Лишние фишки — наружу от клетки.
        let (dr, dc) = outward(r, c);
        for (k, &owner) in owners.iter().skip(1).enumerate() {
            let slot = k + 1;
            if slot > MARGIN {
                grid[shift(pr, dr, MARGIN as isize)][shift(pc, dc, MARGIN as isize)] =
                    Span::raw("+");
                break;
            }
            grid[shift(pr, dr, slot as isize)][shift(pc, dc, slot as isize)] =
                checker_marker(owner);
        }
    }

    grid.into_iter()
        .map(|row| {
            let mut spans = Vec::with_capacity(row.len() * 2);
            for (i, cell) in row.into_iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(cell);
            }
            Line::from(spans)
        })
        .collect()
}

/// Доска без подсветки.
fn board_lines(state: &GameState) -> Vec<Line<'static>> {
    board_lines_hl(state, None, None)
}

/// Панель «вне доски» по каждой стороне.
fn panel_lines(state: &GameState) -> Vec<Line<'static>> {
    state
        .active
        .iter()
        .map(|&side| {
            let own = || state.checkers.iter().filter(move |c| c.owner == side);
            let reserve = own().filter(|c| c.pos == Position::Reserve).count();
            let mut home = String::new();
            for depth in 0..HOME_DEPTH as u8 {
                home.push(if own().any(|c| c.pos == Position::Home { depth }) {
                    '#'
                } else {
                    '_'
                });
            }
            let moon: String = own()
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
            Line::from(vec![
                side_span(side),
                Span::raw(format!(
                    ": резерв {reserve} | дом [{home}] | луна [{moon}] | плен {captured}"
                )),
            ])
        })
        .collect()
}

/// Строки статуса: чей ход, последний ход, победитель, подсказки.
fn status_lines(game: &Game, last: Option<&TurnOutcome>, paused: bool) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::raw("Ход: "),
        side_span(game.state.to_move),
        Span::raw(if paused { "   [ПАУЗА]" } else { "" }),
    ])];

    if let Some(o) = last {
        let [a, b] = o.roll.values();
        let mut spans = vec![Span::raw("Последний: "), side_span(o.side)];
        spans.push(Span::raw(format!(" [{a}+{b}], ходов {}", o.played.len())));
        if o.again {
            spans.push(Span::raw(" • дубль"));
        }
        if !o.forced.is_empty() {
            spans.push(Span::raw(" • выкуп"));
        }
        lines.push(Line::from(spans));
    }

    if let Some(w) = game.winner() {
        lines.push(Line::from(vec![
            Span::raw("Победила сторона "),
            side_span(w),
        ]));
    }

    lines.push(Line::raw(""));
    lines.extend(panel_lines(&game.state));
    lines.push(Line::raw(""));
    lines.push(Line::raw("[space] пауза  [s] шаг  [q] выход"));
    lines
}

/// Рисует один кадр.
fn draw(frame: &mut Frame, game: &Game, last: Option<&TurnOutcome>, paused: bool) {
    let [left, right] =
        Layout::horizontal([Constraint::Length(58), Constraint::Min(20)]).areas(frame.area());
    frame.render_widget(
        Paragraph::new(board_lines(&game.state)).block(Block::bordered().title("Шеш-Беш")),
        left,
    );
    frame.render_widget(
        Paragraph::new(status_lines(game, last, paused)).block(Block::bordered().title("Статус")),
        right,
    );
}

/// Запускает интерактивный TUI: эвристика (или иной `agent`) играет с темпом
/// `step`. Возвращает победителя, если партия завершилась до выхода.
pub fn run<A: Agent>(active: Vec<Side>, agent: &mut A, step: Duration) -> io::Result<Option<Side>> {
    let mut dice = RandomDice::from_entropy();
    let mut game = Game::start(active, &mut dice);
    let mut last: Option<TurnOutcome> = None;
    let mut paused = false;

    let mut terminal = init();
    let result = loop {
        terminal.draw(|f| draw(f, &game, last.as_ref(), paused))?;

        // Ждём событие не дольше `step`; по таймауту — авто-ход.
        if event::poll(step)? {
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(game.winner()),
                    KeyCode::Char(' ') => paused = !paused,
                    KeyCode::Char('s') if game.winner().is_none() => {
                        last = Some(game.play_turn(&mut dice, agent));
                    }
                    _ => {}
                }
            }
        } else if !paused && game.winner().is_none() {
            last = Some(game.play_turn(&mut dice, agent));
        }
    };

    restore();
    result
}

// --- Интерактивная игра человека ---

/// Короткое название характера хода.
fn kind_label(kind: MoveKind) -> &'static str {
    match kind {
        MoveKind::Enter => "ввод",
        MoveKind::Step => "ход",
        MoveKind::EnterMoon => "на Луну",
        MoveKind::MoonAdvance => "Луна+",
        MoveKind::MoonExit => "с Луны",
        MoveKind::EnterPrison => "в Тюрьму",
        MoveKind::PrisonRelease => "из Тюрьмы",
        MoveKind::EnterHome => "в Дом",
        MoveKind::Ransom => "выкуп",
    }
}

/// Применяет последовательность ходов к копии состояния (для превью).
fn preview_after(state: &GameState, seq: &[Move]) -> GameState {
    let mut s = state.clone();
    for &mv in seq {
        s = apply(&s, mv);
    }
    s
}

/// Легенда подсветки: синий — откуда, зелёный — куда.
fn legend_line() -> Line<'static> {
    Line::from(vec![
        Span::styled("  ", Style::default().bg(Color::Blue)),
        Span::raw(" откуда   "),
        Span::styled("  ", Style::default().bg(Color::Green)),
        Span::raw(" куда"),
    ])
}

/// Клетки «откуда» и «куда» для хода (для подсветки на текущей доске).
/// Для входа на Луну/в Дом часть концов вне периметра (`None`).
fn move_endpoints(base: &GameState, mv: Move) -> (Option<PerimeterIdx>, Option<PerimeterIdx>) {
    let owner = base.checkers[mv.checker].owner;
    let from = base.checkers[mv.checker].pos.perimeter_cell(owner);
    let after = preview_after(base, std::slice::from_ref(&mv));
    let to = after.checkers[mv.checker]
        .pos
        .perimeter_cell(owner)
        .or_else(|| {
            // Вход на Луну: фишка уходит на дорожку, но подсветим клетку приземления.
            if let Position::OnTrack { progress } = base.checkers[mv.checker].pos {
                let target = progress as usize + mv.die as usize;
                (target < PERIMETER).then(|| owner.entry().advance(target))
            } else {
                None
            }
        });
    (from, to)
}

/// Человекочитаемое описание варианта хода.
fn describe_turn(seq: &[Move]) -> String {
    if seq.is_empty() {
        return "(пропуск — ходов нет)".to_string();
    }
    let pips: u32 = seq.iter().map(|m| m.die as u32).sum();
    let parts: Vec<String> = seq
        .iter()
        .map(|m| format!("{}·{}", m.die, kind_label(m.kind)))
        .collect();
    format!("{pips} очк.: {}", parts.join(", "))
}

/// Строки списка вариантов с подсветкой выбранного.
fn option_lines(descs: &[String], sel: usize) -> Vec<Line<'static>> {
    descs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let style = if i == sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Line::styled(format!(" {:>2}. {d} ", i + 1), style)
        })
        .collect()
}

/// Рисует экран выбора: слева заголовок и список вариантов, справа текущая
/// доска с подсветкой выделенного хода (`from`/`to`).
fn draw_pick(
    frame: &mut Frame,
    board: &GameState,
    from: Option<PerimeterIdx>,
    to: Option<PerimeterIdx>,
    board_title: &str,
    header: &[Line<'static>],
    options: Vec<Line<'static>>,
) {
    let [left, right] =
        Layout::horizontal([Constraint::Length(34), Constraint::Min(0)]).areas(frame.area());
    let mut lines = header.to_vec();
    lines.push(Line::raw(""));
    lines.extend(options);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("Выбор хода")),
        left,
    );
    frame.render_widget(
        Paragraph::new(board_lines_hl(board, from, to))
            .block(Block::bordered().title(board_title.to_string())),
        right,
    );
}

/// Навигация по списку клавишами; `Some(i)` — выбран, `None` — выход (`q`/`Esc`).
fn select_loop(
    terminal: &mut DefaultTerminal,
    len: usize,
    allow_quit: bool,
    mut render: impl FnMut(&mut Frame, usize),
) -> io::Result<Option<usize>> {
    let mut sel = 0usize;
    loop {
        terminal.draw(|f| render(f, sel))?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => sel = (sel + len - 1) % len,
                KeyCode::Down | KeyCode::Char('j') => sel = (sel + 1) % len,
                KeyCode::Enter => return Ok(Some(sel)),
                KeyCode::Char('q') | KeyCode::Esc if allow_quit => return Ok(None),
                _ => {}
            }
        }
    }
}

/// Уникальные ходы на шаге `step` среди последовательностей с префиксом `prefix`.
/// Так выбор человека всегда остаётся на пути к ходу из `legal_turns`.
fn step_options(turns: &[Vec<Move>], prefix: &[Move], step: usize) -> Vec<Move> {
    let mut options: Vec<Move> = Vec::new();
    for seq in turns
        .iter()
        .filter(|t| t.len() > step && t[..step] == *prefix)
    {
        if !options.contains(&seq[step]) {
            options.push(seq[step]);
        }
    }
    options
}

/// Человек собирает ход по одной кости за шаг, оставаясь на пути к ходу с
/// максимумом очков. Возвращает выбранную полную последовательность или `None`
/// (игрок вышел). Все варианты в `turns` имеют одну длину (см. `legal_turns`):
/// если обе кости играбельны — это всегда два хода, иначе один (или ноль).
fn human_pick_sequence(
    terminal: &mut DefaultTerminal,
    state: &GameState,
    roll: DiceRoll,
    turns: &[Vec<Move>],
) -> io::Result<Option<Vec<Move>>> {
    let total = turns[0].len();
    let [a, b] = roll.values();

    if total == 0 {
        // Ходов нет — показываем экран и ждём подтверждения.
        let header = vec![
            Line::from(vec![
                Span::raw("Ваш ход: "),
                side_span(state.to_move),
                Span::raw(format!("   бросок [{a}+{b}]")),
            ]),
            Line::raw("Ходов нет. Enter — пропустить, q — выход"),
        ];
        let labels = vec!["(пропуск — ходов нет)".to_string()];
        let picked = select_loop(terminal, 1, true, |f, sel| {
            draw_pick(
                f,
                state,
                None,
                None,
                "Текущая позиция",
                &header,
                option_lines(&labels, sel),
            );
        })?;
        return Ok(picked.map(|_| Vec::new()));
    }

    let mut prefix: Vec<Move> = Vec::new();
    let mut base = state.clone();

    for step in 0..total {
        // Варианты-ходы на этом шаге среди последовательностей с уже выбранным префиксом.
        let options = step_options(turns, &prefix, step);

        let labels: Vec<String> = options
            .iter()
            .map(|m| format!("{}·{}", m.die, kind_label(m.kind)))
            .collect();
        let chosen_desc = if prefix.is_empty() {
            "—".to_string()
        } else {
            describe_turn(&prefix)
        };
        let header = vec![
            Line::from(vec![
                Span::raw("Ваш ход: "),
                side_span(state.to_move),
                Span::raw(format!("   бросок [{a}+{b}]")),
            ]),
            Line::raw(format!(
                "Кость {}/{}   выбрано: {chosen_desc}",
                step + 1,
                total
            )),
            legend_line(),
            Line::raw("↑/↓ — выбор, Enter — применить, q — выход"),
        ];
        let title = format!("Позиция (кость {}/{})", step + 1, total);

        let picked = select_loop(terminal, options.len(), true, |f, sel| {
            let (from, to) = move_endpoints(&base, options[sel]);
            draw_pick(
                f,
                &base,
                from,
                to,
                &title,
                &header,
                option_lines(&labels, sel),
            );
        })?;
        let Some(i) = picked else {
            return Ok(None);
        };
        prefix.push(options[i]);
        base = preview_after(&base, std::slice::from_ref(&options[i]));
    }

    Ok(Some(prefix))
}

/// Человек выбирает вынужденный ответный ход на «6» при выкупе.
fn human_pick_forced(
    terminal: &mut DefaultTerminal,
    state: &GameState,
    captor: Side,
    options: &[Move],
) -> io::Result<usize> {
    let descs: Vec<String> = options
        .iter()
        .map(|m| format!("{}·{}", m.die, kind_label(m.kind)))
        .collect();
    let header = vec![
        Line::from(vec![
            Span::raw("Выкуп — ваш обязательный ход на 6: "),
            side_span(captor),
        ]),
        legend_line(),
        Line::raw("↑/↓ — выбор, Enter — применить"),
    ];
    let idx = select_loop(terminal, options.len(), false, |f, sel| {
        let (from, to) = move_endpoints(state, options[sel]);
        draw_pick(
            f,
            state,
            from,
            to,
            "Текущая позиция",
            &header,
            option_lines(&descs, sel),
        );
    })?;
    Ok(idx.unwrap_or(0))
}

/// Показывает финальный экран и ждёт `q`/`Esc`/`Enter`.
fn show_final(
    terminal: &mut DefaultTerminal,
    game: &Game,
    last: Option<&TurnOutcome>,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| draw(f, game, last, false))?;
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter)
        {
            return Ok(());
        }
    }
}

/// Интерактивная партия: стороны из `humans` ходит человек, остальные — `ai`.
/// Возвращает победителя, если партия завершилась (а не прервана выходом).
pub fn run_interactive<A: Agent>(
    active: Vec<Side>,
    humans: Vec<Side>,
    ai: &mut A,
    step: Duration,
) -> io::Result<Option<Side>> {
    let mut dice = RandomDice::from_entropy();
    let mut game = Game::start(active, &mut dice);
    let mut terminal = init();
    let mut last: Option<TurnOutcome> = None;

    let result = (|| -> io::Result<Option<Side>> {
        loop {
            if let Some(winner) = game.winner() {
                show_final(&mut terminal, &game, last.as_ref())?;
                return Ok(Some(winner));
            }

            let side = game.state.to_move;
            let roll = dice.roll();
            let turns = legal_turns(&game.state, roll);

            let chosen = if humans.contains(&side) {
                match human_pick_sequence(&mut terminal, &game.state, roll, &turns)? {
                    Some(seq) => seq,
                    None => return Ok(None), // игрок вышел
                }
            } else {
                let i = ai.choose_turn(&game.state, &turns).min(turns.len() - 1);
                turns[i].clone()
            };

            let outcome = {
                let humans = &humans;
                let term = &mut terminal;
                let ai = &mut *ai;
                game.commit_turn(roll, chosen, |state, captor, options| {
                    if humans.contains(&captor) {
                        human_pick_forced(term, state, captor, options).unwrap_or(0)
                    } else {
                        ai.choose_forced(state, captor, options)
                    }
                })
            };
            last = Some(outcome);

            // После хода ИИ — пауза, чтобы человек увидел результат.
            if !humans.contains(&side) {
                terminal.draw(|f| draw(f, &game, last.as_ref(), false))?;
                thread::sleep(step);
            }
        }
    })();

    restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Содержимое всех спанов строк одной плоской строкой.
    fn flatten(lines: &[Line]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn board_lines_have_landmarks_and_colored_checker() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        state.checkers[0].pos = Position::OnTrack { progress: 0 };
        let lines = board_lines(&state);
        let text = flatten(&lines);
        for mark in ['+', 'M', 'J', 'h'] {
            assert!(text.contains(mark), "нет landmark {mark}");
        }
        // Фишка A окрашена в свой цвет.
        let a_span = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.starts_with('A'))
            .expect("должен быть спан с фишкой A");
        assert_eq!(a_span.style.fg, Some(side_color(Side::A)));
    }

    #[test]
    fn board_has_outer_margins() {
        let state = GameState::new(vec![Side::A, Side::C], Side::A);
        // Сетка увеличена на поля с каждой стороны.
        assert_eq!(board_lines(&state).len(), SIDE_LEN + 2 * MARGIN);
    }

    #[test]
    fn multiple_checkers_spill_outside_the_field() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        // Три фишки A на одной клетке (точка входа A) — все должны быть видны.
        for c in state
            .checkers
            .iter_mut()
            .filter(|c| c.owner == Side::A)
            .take(3)
        {
            c.pos = Position::OnTrack { progress: 0 };
        }
        let text = flatten(&board_lines(&state));
        // На доске буква A встречается только как маркер фишки → не меньше трёх.
        assert!(text.matches('A').count() >= 3);
    }

    #[test]
    fn describe_turn_summarises_moves() {
        use crate::dice::{DiceRoll, Die};
        let state = GameState::new(vec![Side::A, Side::C], Side::A);
        // Бросок (6,6): ввод двух фишек — оба хода по «6».
        let roll = DiceRoll::new(Die::new(6).unwrap(), Die::new(6).unwrap());
        let turns = legal_turns(&state, roll);
        let with_enter = turns
            .iter()
            .find(|t| t.iter().all(|m| m.kind == MoveKind::Enter) && !t.is_empty())
            .expect("должен быть ход с вводом");
        let text = describe_turn(with_enter);
        assert!(text.contains("очк."));
        assert!(text.contains("ввод"));
        // Пустой ход описывается как пропуск.
        assert!(describe_turn(&[]).contains("пропуск"));
    }

    #[test]
    fn two_step_options_stay_on_legal_paths() {
        use crate::dice::{DiceRoll, Die};
        let state = GameState::new(vec![Side::A, Side::C], Side::A);
        // Дубль 6-6: можно ввести две фишки — ход из двух «шагов».
        let roll = DiceRoll::new(Die::new(6).unwrap(), Die::new(6).unwrap());
        let turns = legal_turns(&state, roll);
        // Все варианты одной длины (правило максимального хода → обе кости).
        assert!(turns.iter().all(|t| t.len() == turns[0].len()));
        assert_eq!(turns[0].len(), 2);

        let first = step_options(&turns, &[], 0);
        assert!(!first.is_empty());
        let m0 = first[0];
        let second = step_options(&turns, std::slice::from_ref(&m0), 1);
        assert!(!second.is_empty());
        // Собранная из шагов последовательность — легальный полный ход.
        assert!(turns.contains(&vec![m0, second[0]]));
    }

    #[test]
    fn option_lines_highlight_selection() {
        let descs = vec!["один".to_string(), "два".to_string()];
        let lines = option_lines(&descs, 1);
        assert_eq!(lines.len(), 2);
        // Выбранная строка — инверсная.
        assert!(lines[1].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!lines[0].style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn status_shows_turn_and_winner() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        for (depth, c) in state
            .checkers
            .iter_mut()
            .filter(|c| c.owner == Side::A)
            .enumerate()
        {
            c.pos = Position::Home { depth: depth as u8 };
        }
        let game = Game::new(state);
        let text = flatten(&status_lines(&game, None, false));
        assert!(text.contains("Ход: "));
        assert!(text.contains("Победила сторона"));
        assert!(text.contains("[space] пауза"));
    }
}
