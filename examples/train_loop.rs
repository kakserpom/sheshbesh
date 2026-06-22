//! Бесконечное self-play обучение «поколениями». Каждое поколение ПРОДОЛЖАЕТ обучение
//! из текущей ЛУЧШЕЙ модели (тёплый старт) и принимается, только если стало играть
//! сильнее (гейтинг по доле побед против эвристики на ОБЩЕМ наборе бросков). Так модель
//! монотонно улучшается и не деградирует от дрейфа self-play-политики. Лучшие веса
//! каждого режима сохраняются в `frontend/src/model*.bin` (после — `trunk build`, чтобы
//! GUI подхватил их).
//!
//! Запуск: `cargo run --release --example train_loop`  (Ctrl+C — остановить).
//! Аргументы (опц.): `<поколений> <партий-обучения> <партий-оценки>`.
//! Без `<поколений>` — бесконечно. Пример быстрой проверки: `… train_loop 1 500 100`.

use sheshbesh::{
    Heuristic, LinearValue, Side, TdConfig, ValueAgent, match_winrate, train_with, winrate_ffa,
    winrate_teams,
};
use std::io::Write;

struct Mode {
    name: &'static str,
    file: &'static str,
    active: Vec<Side>,
    teams: bool,
}

fn clone_model(m: &LinearValue) -> LinearValue {
    LinearValue::from_floats(&m.to_floats())
}

fn load_or_zero(path: &str) -> LinearValue {
    std::fs::read(path).ok().map_or_else(
        || LinearValue::from_floats(&[0.0_f32; sheshbesh::FEATURES + 1]),
        |b| {
            let f: Vec<f32> = b
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            LinearValue::from_floats(&f)
        },
    )
}

fn save(path: &str, m: &LinearValue) {
    let mut bytes = Vec::new();
    for f in m.to_floats() {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write model");
}

/// Сила модели = доля побед против эвристики на фиксированном `seed` (для сравнимости
/// кандидата и лучшего на одних и тех же бросках).
fn strength(m: &LinearValue, mode: &Mode, games: usize, seed: u64) -> f64 {
    if mode.active.len() == 2 {
        let (w, h) = match_winrate(&mut ValueAgent(m), &mut Heuristic, games, seed, 40_000);
        return w as f64 / (w + h).max(1) as f64;
    }
    let (w, g) = if mode.teams {
        winrate_teams(m, games, seed, 30_000)
    } else {
        winrate_ffa(m, &mode.active, games, seed, 30_000)
    };
    w as f64 / g.max(1) as f64
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let max_gens: Option<u64> = args.get(1).and_then(|s| s.parse().ok());
    let train_games: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10_000);
    let eval_games: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(800);
    // Принимаем кандидата только при перевесе НЕ меньше этого порога — иначе шум оценки
    // (винрейт в нардах гуляет на ±несколько %) гонял бы веса случайным блужданием.
    let margin = 0.02_f64;

    let modes = [
        Mode { name: "2p", file: "frontend/src/model.bin", active: vec![Side::A, Side::C], teams: false },
        Mode { name: "3p", file: "frontend/src/model_3p.bin", active: vec![Side::A, Side::B, Side::C], teams: false },
        Mode { name: "4p", file: "frontend/src/model_4p.bin", active: Side::ALL.to_vec(), teams: false },
        Mode { name: "4p-teams", file: "frontend/src/model_4p_teams.bin", active: Side::ALL.to_vec(), teams: true },
    ];

    let mut best: Vec<LinearValue> = modes.iter().map(|m| load_or_zero(m.file)).collect();
    for (md, m) in modes.iter().zip(&best) {
        println!("[generation 0] {:>8}: старт {:.1}% против эвристики", md.name, 100.0 * strength(m, md, eval_games, 777));
    }
    std::io::stdout().flush().ok();

    let mut generation = 1u64;
    loop {
        if max_gens.is_some_and(|n| generation > n) {
            break;
        }
        for (i, md) in modes.iter().enumerate() {
            // Кандидат: продолжаем обучение из лучшей модели со свежим seed (исследование).
            let cfg = TdConfig {
                games: train_games,
                max_turns: 30_000,
                seed: 0x5DEE_CE66 ^ generation.wrapping_mul(2_654_435_761) ^ i as u64,
                active: md.active.clone(),
                teams: md.teams,
                ..Default::default()
            };
            let cand = train_with(clone_model(&best[i]), &cfg);
            // Гейтинг: кандидат и лучший — на ОДНОМ наборе бросков (общий eval-seed).
            let eval_seed = 1_000 + generation;
            let cs = strength(&cand, md, eval_games, eval_seed);
            let bs = strength(&best[i], md, eval_games, eval_seed);
            let mark = if cs > bs + margin {
                best[i] = cand;
                save(md.file, &best[i]);
                "ПРИНЯТА → сохранена"
            } else {
                "отклонена"
            };
            println!("[generation {generation}] {:>8}: кандидат {:.1}% vs лучший {:.1}% — {mark}", md.name, 100.0 * cs, 100.0 * bs);
            std::io::stdout().flush().ok();
        }
        generation += 1;
    }
}
