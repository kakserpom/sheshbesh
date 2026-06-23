//! TUI-дашборд обучения MLP: live-прогресс по 4 режимам и винрейт против эвристики.
//! Обучение идёт в фоновом потоке, главный поток рисует прогресс (ratatui).
//!
//! Запуск: `cargo run --release --example train_tui [партий]` (по умолчанию 18000).
//! Клавиша `q`/`Esc` — выйти (обученные к этому моменту режимы уже сохранены).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Gauge, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use sheshbesh::{
    Heuristic, MlpValue, Side, TdConfig, ValueAgent, match_winrate, train_with_progress,
    winrate_ffa, winrate_teams,
};

const HIDDEN: usize = 24;

struct Mode {
    name: &'static str,
    file: &'static str,
    active: Vec<Side>,
    teams: bool,
    fair: f64, // честная доля, % — для подписи
}

struct Shared {
    games: usize,
    done: Vec<AtomicUsize>, // сыграно партий на режим
    winr: Vec<AtomicU32>,   // винрейт ×10 после обучения (0 — ещё нет)
    cur: AtomicUsize,       // индекс обучаемого режима
    finished: AtomicBool,
}

fn save(path: &str, m: &MlpValue) {
    let mut bytes = Vec::new();
    for f in m.to_floats() {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write model");
}

fn strength(m: &MlpValue, mode: &Mode) -> f64 {
    if mode.active.len() == 2 {
        let (w, h) = match_winrate(&mut ValueAgent(m), &mut Heuristic, 400, 4242, 40_000);
        return 100.0 * w as f64 / (w + h).max(1) as f64;
    }
    let (w, g) = if mode.teams {
        winrate_teams(m, 400, 4242, 30_000)
    } else {
        winrate_ffa(m, &mode.active, 400, 4242, 30_000)
    };
    100.0 * w as f64 / g.max(1) as f64
}

fn draw(f: &mut Frame, modes: &[Mode], sh: &Shared, started: Instant) {
    let n = modes.len();
    let mut cons: Vec<Constraint> = (0..n).map(|_| Constraint::Length(3)).collect();
    cons.push(Constraint::Length(1));
    let areas = Layout::vertical(cons).split(f.area());
    let cur = sh.cur.load(Ordering::Relaxed);
    let finished = sh.finished.load(Ordering::Relaxed);
    for (i, md) in modes.iter().enumerate() {
        let done = sh.done[i].load(Ordering::Relaxed).min(sh.games);
        let ratio = done as f64 / sh.games as f64;
        let wr = sh.winr[i].load(Ordering::Relaxed);
        let label = if wr > 0 {
            format!(
                "{done}/{} — {:.1}% против эвристики (честно {:.0}%)",
                sh.games,
                wr as f64 / 10.0,
                md.fair
            )
        } else if i == cur && !finished {
            format!("{done}/{} — обучается…", sh.games)
        } else {
            format!("{done}/{}", sh.games)
        };
        let color = if wr > 0 { Color::Green } else { Color::Cyan };
        let g = Gauge::default()
            .block(Block::bordered().title(format!(" {} ", md.name)))
            .gauge_style(Style::default().fg(color).add_modifier(Modifier::BOLD))
            .ratio(ratio.min(1.0))
            .label(label);
        f.render_widget(g, areas[i]);
    }
    let secs = started.elapsed().as_secs();
    let status = if finished {
        format!("Готово за {secs} с. Модели сохранены. q — выход.")
    } else {
        format!("Обучение MLP (h={HIDDEN}) — {secs} с… q — выход")
    };
    f.render_widget(Paragraph::new(Line::from(status)), areas[n]);
}

fn run_ui(term: &mut DefaultTerminal, modes: &[Mode], sh: &Shared) -> std::io::Result<()> {
    let started = Instant::now();
    loop {
        term.draw(|f| draw(f, modes, sh, started))?;
        if event::poll(Duration::from_millis(120))?
            && let Event::Key(k) = event::read()?
            && matches!(k.code, KeyCode::Char('q') | KeyCode::Esc)
        {
            return Ok(());
        }
    }
}

fn main() -> std::io::Result<()> {
    let games: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(18_000);
    let modes = Arc::new(vec![
        Mode {
            name: "2p",
            file: "frontend/src/model.bin",
            active: vec![Side::A, Side::C],
            teams: false,
            fair: 50.0,
        },
        Mode {
            name: "3p",
            file: "frontend/src/model_3p.bin",
            active: vec![Side::A, Side::B, Side::C],
            teams: false,
            fair: 33.3,
        },
        Mode {
            name: "4p",
            file: "frontend/src/model_4p.bin",
            active: Side::ALL.to_vec(),
            teams: false,
            fair: 25.0,
        },
        Mode {
            name: "4p-teams",
            file: "frontend/src/model_4p_teams.bin",
            active: Side::ALL.to_vec(),
            teams: true,
            fair: 50.0,
        },
    ]);
    let sh = Arc::new(Shared {
        games,
        done: (0..modes.len()).map(|_| AtomicUsize::new(0)).collect(),
        winr: (0..modes.len()).map(|_| AtomicU32::new(0)).collect(),
        cur: AtomicUsize::new(0),
        finished: AtomicBool::new(false),
    });

    // Фоновый поток: обучает режимы по очереди, обновляя общий прогресс.
    let (mw, shw) = (Arc::clone(&modes), Arc::clone(&sh));
    let worker = std::thread::spawn(move || {
        for (i, md) in mw.iter().enumerate() {
            shw.cur.store(i, Ordering::Relaxed);
            let cfg = TdConfig {
                games: shw.games,
                max_turns: 30_000,
                active: md.active.clone(),
                teams: md.teams,
                ..Default::default()
            };
            let model = train_with_progress(MlpValue::new(HIDDEN, 1), &cfg, &shw.done[i]);
            let wr = strength(&model, md);
            shw.winr[i].store((wr * 10.0) as u32, Ordering::Relaxed);
            save(md.file, &model);
        }
        shw.finished.store(true, Ordering::Relaxed);
    });

    let mut term = ratatui::init();
    let res = run_ui(&mut term, &modes, &sh);
    ratatui::restore();
    if sh.finished.load(Ordering::Relaxed) {
        let _ = worker.join();
    }
    res
}
