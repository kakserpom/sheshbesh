//! Сравнивает два набора обученных моделей **голова-в-голову** по всем 4 режимам.
//! Каждый режим грузит файл `frontend/src/{prefix}{tag}.bin` для каждого набора (число
//! нейронов выводится из длины весов, так что наборы могут быть разного размера) и
//! играет матч A vs B. Для контекста печатает и силу каждого против эвристики.
//!
//! Запуск: `cargo run --release --example compare <prefixA> <prefixB> [партий] [сидов]`
//! Пример: `cargo run --release --example compare model model_h48 1000 4`
//!   → сравнивает базовые (h=24) модели с экспериментальными (prefix `model_h48`).

use sheshbesh::{
    FEATURES, Heuristic, MlpValue, Side, ValueAgent, match_winrate, winrate_ffa, winrate_ffa_vs,
    winrate_teams, winrate_teams_vs,
};

struct Mode {
    name: &'static str,
    tag: &'static str,
    active: Vec<Side>,
    teams: bool,
}

fn load(path: &str) -> Option<MlpValue> {
    let bytes = std::fs::read(path).ok()?;
    let f: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Some(MlpValue::from_floats(&f))
}

/// Число скрытых нейронов: всего весов = h·FEATURES + h (b1) + h (w2) + 1 (b2)
/// = h·(FEATURES+2) + 1, отсюда h = (len−1)/(FEATURES+2).
fn hidden_of(m: &MlpValue) -> usize {
    m.to_floats().len().saturating_sub(1) / (FEATURES + 2)
}

/// Доля побед A против B (голова-в-голову) для режима — как гейтинг в `train_loop`.
fn duel(a: &MlpValue, b: &MlpValue, mode: &Mode, games: usize, seed: u64) -> f64 {
    if mode.active.len() == 2 {
        let (aw, bw) = match_winrate(&mut ValueAgent(a), &mut ValueAgent(b), games, seed, 40_000);
        return aw as f64 / (aw + bw).max(1) as f64;
    }
    let (w, g) = if mode.teams {
        winrate_teams_vs(a, b, games, seed, 30_000)
    } else {
        winrate_ffa_vs(a, b, &mode.active, games, seed, 30_000)
    };
    w as f64 / g.max(1) as f64
}

/// Сила модели против эвристики (для контекста).
fn vs_heuristic(m: &MlpValue, mode: &Mode, games: usize, seed: u64) -> f64 {
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

fn avg(f: impl Fn(u64) -> f64, base: u64, n_seeds: usize) -> f64 {
    let n = n_seeds.max(1);
    (0..n)
        .map(|i| f(base.wrapping_add((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))))
        .sum::<f64>()
        / n as f64
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let prefix_a = args.get(1).cloned().unwrap_or_else(|| "model".to_string());
    let prefix_b = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "model_h48".to_string());
    let games: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let n_seeds: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(4);

    let modes = [
        Mode {
            name: "2p",
            tag: "",
            active: vec![Side::A, Side::C],
            teams: false,
        },
        Mode {
            name: "3p",
            tag: "_3p",
            active: vec![Side::A, Side::B, Side::C],
            teams: false,
        },
        Mode {
            name: "4p",
            tag: "_4p",
            active: Side::ALL.to_vec(),
            teams: false,
        },
        Mode {
            name: "4p-teams",
            tag: "_4p_teams",
            active: Side::ALL.to_vec(),
            teams: true,
        },
    ];

    println!(
        "Сравнение голова-в-голову: A=`{prefix_a}` против B=`{prefix_b}` \
         ({games} партий × {n_seeds} сидов)\n"
    );
    for md in &modes {
        let pa = format!("frontend/src/{prefix_a}{}.bin", md.tag);
        let pb = format!("frontend/src/{prefix_b}{}.bin", md.tag);
        let (Some(a), Some(b)) = (load(&pa), load(&pb)) else {
            println!("{:>8}: пропуск — нет файла ({pa} или {pb})", md.name);
            continue;
        };
        let base = 1_000;
        let d = avg(|s| duel(&a, &b, md, games, s), base, n_seeds);
        let ha = avg(|s| vs_heuristic(&a, md, games, s), base, n_seeds);
        let hb = avg(|s| vs_heuristic(&b, md, games, s), base, n_seeds);
        let fair = if md.active.len() == 2 || md.teams {
            50.0
        } else {
            100.0 / md.active.len() as f64
        };
        let verdict = if d > fair / 100.0 + 0.01 {
            "A сильнее"
        } else if d < fair / 100.0 - 0.01 {
            "B сильнее"
        } else {
            "~равны"
        };
        println!(
            "{:>8}: дуэль A={:.1}% (честно {:.1}%) — {verdict}; против эвристики A={:.1}% B={:.1}% \
             [нейронов A={} B={}]",
            md.name,
            100.0 * d,
            fair,
            100.0 * ha,
            100.0 * hb,
            hidden_of(&a),
            hidden_of(&b),
        );
    }
}
