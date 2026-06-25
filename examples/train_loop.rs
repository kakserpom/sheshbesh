//! Бесконечное self-play обучение «поколениями». Каждое поколение ПРОДОЛЖАЕТ обучение
//! из текущей ЛУЧШЕЙ модели (тёплый старт) и принимается, только если стало играть
//! сильнее (гейтинг по доле побед против эвристики на ОБЩЕМ наборе бросков). Так модель
//! монотонно улучшается и не деградирует от дрейфа self-play-политики. Лучшие веса
//! каждого режима сохраняются в `frontend/src/model*.bin` (после — `trunk build`, чтобы
//! GUI подхватил их).
//!
//! Запуск: `cargo run --release --example train_loop`.
//! Аргументы (опц.): `<поколений> <партий-обучения> <партий-оценки> <сидов-оценки>
//! <нейронов> <префикс-файлов> <alpha>`.
//! Без `<поколений>` — бесконечно. `<сидов-оценки>` (по умолчанию 4) — по скольки разным
//! наборам бросков усредняется сила при гейтинге. `<нейронов>` (по умолчанию 24) — размер
//! скрытого слоя при ХОЛОДНОМ старте; чтобы обучить сеть другого размера, **обязательно**
//! задай и **другой** `<префикс-файлов>` (по умолчанию `model`), иначе тёплый старт
//! подхватит старые веса со старым размером. `<alpha>` (по умолчанию ~0.1) — начальный шаг
//! обучения; крупной сети с холодного старта полезен **больший α + больше партий**.
//! Пример для 48 нейронов: `… train_loop "" 20000 800 4 48 model_h48 0.15`
//! (пустой 1-й арг = бесконечно). Быстрая проверка: `… train_loop 1 500 100 2`.
//!
//! **Штатная остановка:** Ctrl+C — доигрывает текущий режим (чтобы не бросать
//! недосчитанную модель) и выходит из цикла; повторный Ctrl+C — выход немедленно.
//! Сохранение **атомарное** (временный файл → `rename`), поэтому прерывание не может
//! оставить повреждённый `.bin`.

use sheshbesh::{
    FEATURES, Heuristic, MlpValue, Side, TdConfig, ValueAgent, match_winrate, train_with,
    winrate_ffa, winrate_ffa_vs, winrate_teams, winrate_teams_vs,
};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Скрытых нейронов MLP по умолчанию (как в `export_model.rs`); можно переопределить
/// аргументом — при холодном старте сеть создаётся с этим числом нейронов.
const HIDDEN: usize = 48;

struct Mode {
    name: &'static str,
    /// Суффикс файла модели режима: пусто для 2p, иначе `_3p` / `_4p` / `_4p_teams`.
    tag: &'static str,
    active: Vec<Side>,
    teams: bool,
}

/// Путь к файлу весов: `frontend/src/{prefix}{tag}.bin`. `prefix` по умолчанию `model`
/// (файлы GUI); для экспериментов с другим числом нейронов берётся свой prefix, чтобы
/// не затирать базовые модели.
fn model_path(prefix: &str, tag: &str) -> String {
    format!("frontend/src/{prefix}{tag}.bin")
}

fn clone_model(m: &MlpValue) -> MlpValue {
    MlpValue::from_floats(&m.to_floats())
}

/// Загружает модель из файла (число нейронов выводится из длины весов) либо создаёт
/// новую с `hidden` нейронами (холодный старт). Файл **несовместимой** длины (напр. после
/// изменения числа признаков в `encode.rs`) молча игнорируется — холодный старт вместо
/// паники в `from_floats`. ВНИМАНИЕ: совместимый файл задаёт свой `hidden` (тёплый старт);
/// для нового размера сети нужен **новый** prefix.
fn load_or_init(path: &str, hidden: usize) -> MlpValue {
    let from_file = std::fs::read(path).ok().and_then(|b| {
        let f: Vec<f32> = b
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        // Совместим ли файл с ТЕКУЩИМ FEATURES? len = h·FEATURES + 2·h + 1.
        let h = f.len().checked_sub(1)? / (FEATURES + 2);
        (h > 0 && h * FEATURES + 2 * h + 1 == f.len()).then(|| MlpValue::from_floats(&f))
    });
    from_file.unwrap_or_else(|| MlpValue::new(hidden, 1))
}

/// Атомарное сохранение: пишем во временный файл рядом, затем `rename` (на той же ФС
/// — атомарная замена). Прерывание в любой момент оставляет либо старый, либо новый
/// целый файл, но не обрезанный.
fn save(path: &str, m: &MlpValue) {
    let mut bytes = Vec::new();
    for f in m.to_floats() {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, &bytes).expect("write model (tmp)");
    std::fs::rename(&tmp, path).expect("rename model into place");
}

/// Сила модели = доля побед против эвристики на фиксированном `seed` (для сравнимости
/// кандидата и лучшего на одних и тех же бросках).
fn strength(m: &MlpValue, mode: &Mode, games: usize, seed: u64) -> f64 {
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

/// `i`-й оценочный seed, выведенный из базового `base` (различные, детерминированные).
fn nth_seed(base: u64, i: usize) -> u64 {
    base.wrapping_add((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

/// Сила, усреднённая по `n_seeds` РАЗНЫМ наборам бросков (выведенным из `base`). Снижает
/// выборочный шум винрейта ≈ в √`n_seeds` раз, поэтому гейтинг сравнивает кандидата и
/// лучшего надёжнее (мелкое реальное улучшение перестаёт тонуть в шуме одного набора).
/// Кандидат и лучший должны звать это с ОДНИМ `base` — тогда они меряются на одних и тех
/// же наборах (честно).
fn strength_avg(m: &MlpValue, mode: &Mode, games: usize, base: u64, n_seeds: usize) -> f64 {
    let n = n_seeds.max(1);
    (0..n)
        .map(|i| strength(m, mode, games, nth_seed(base, i)))
        .sum::<f64>()
        / n as f64
}

/// «Честная» доля для головы-в-голову: 0.5 для 2p и команд, 1/n для FFA (кандидат на
/// одном месте против `best` на остальных). Кандидат сильнее лучшего ⇒ доля выше этой.
fn fair_share(mode: &Mode) -> f64 {
    if mode.active.len() == 2 || mode.teams {
        0.5
    } else {
        1.0 / mode.active.len() as f64
    }
}

/// Доля побед кандидата **против самого лучшего** (а не эвристики) на наборе `seed`.
/// 2p — прямой матч; команды — команда кандидата vs команда лучшего; FFA — кандидат на
/// одном (чередующемся) месте, `best` на остальных.
fn head_to_head(cand: &MlpValue, best: &MlpValue, mode: &Mode, games: usize, seed: u64) -> f64 {
    if mode.active.len() == 2 {
        let (cw, bw) =
            match_winrate(&mut ValueAgent(cand), &mut ValueAgent(best), games, seed, 40_000);
        return cw as f64 / (cw + bw).max(1) as f64;
    }
    let (w, g) = if mode.teams {
        winrate_teams_vs(cand, best, games, seed, 30_000)
    } else {
        winrate_ffa_vs(cand, best, &mode.active, games, seed, 30_000)
    };
    w as f64 / g.max(1) as f64
}

/// Голова-в-голову, усреднённая по `n_seeds` наборам (как `strength_avg`).
fn head_to_head_avg(
    cand: &MlpValue,
    best: &MlpValue,
    mode: &Mode,
    games: usize,
    base: u64,
    n_seeds: usize,
) -> f64 {
    let n = n_seeds.max(1);
    (0..n)
        .map(|i| head_to_head(cand, best, mode, games, nth_seed(base, i)))
        .sum::<f64>()
        / n as f64
}

#[allow(clippy::too_many_lines)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let max_gens: Option<u64> = args.get(1).and_then(|s| s.parse().ok());
    let train_games: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10_000);
    let eval_games: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(800);
    // Сколько РАЗНЫХ наборов бросков усреднять при оценке (гейтинг тем надёжнее, чем
    // больше; цена — линейно дольше оценка). Итого на сравнение — `eval_games×eval_seeds`.
    let eval_seeds: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(4);
    // Число нейронов скрытого слоя (для ХОЛОДНОГО старта). Эксперимент «больше нейронов»:
    // задать другое значение И другой `prefix`, чтобы не затирать базовые модели.
    let hidden: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(HIDDEN);
    // Префикс файлов весов (по умолчанию `model` — те, что грузит GUI).
    let prefix: String = args.get(6).cloned().unwrap_or_else(|| "model".to_string());
    // Начальный шаг обучения α (по умолчанию как в `TdConfig`). Крупной сети с холодного
    // старта обычно полезен чуть больший α (быстрее учится) + больше партий на поколение.
    let alpha: f32 = args
        .get(7)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| TdConfig::default().alpha);
    // Принимаем кандидата только при перевесе НЕ меньше этого порога — иначе шум оценки
    // (винрейт в нардах гуляет на ±несколько %) гонял бы веса случайным блужданием.
    let margin = 0.02_f64;

    // Штатная остановка по Ctrl+C: первый сигнал ставит флаг (цикл выйдет на ближайшей
    // границе режима), второй — выходит немедленно.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || {
            if stop.swap(true, Ordering::SeqCst) {
                std::process::exit(130);
            }
            eprintln!(
                "\n[stop] Ctrl+C: доигрываю текущий режим и останавливаюсь (ещё раз — выход сразу)"
            );
        })
        .expect("install Ctrl+C handler");
    }

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

    let mut best: Vec<MlpValue> = modes
        .iter()
        .map(|m| load_or_init(&model_path(&prefix, m.tag), hidden))
        .collect();
    println!(
        "[config] prefix={prefix} hidden(cold-start)={hidden} α={alpha} train={train_games} \
         eval={eval_games}×{eval_seeds} сидов margin={:.0}%",
        margin * 100.0
    );
    for (md, m) in modes.iter().zip(&best) {
        println!(
            "[generation 0] {:>8}: старт {:.1}% против эвристики",
            md.name,
            100.0 * strength_avg(m, md, eval_games, 777, eval_seeds)
        );
    }
    std::io::stdout().flush().ok();

    let mut generation = 1u64;
    'gens: loop {
        if max_gens.is_some_and(|n| generation > n) {
            break;
        }
        for (i, md) in modes.iter().enumerate() {
            // Остановка по Ctrl+C — на границе режима (текущий доигран и сохранён).
            if stop.load(Ordering::SeqCst) {
                println!("[stop] остановлено по запросу (поколение {generation})");
                break 'gens;
            }
            // Кандидат: продолжаем обучение из лучшей модели со свежим seed (исследование).
            let cfg = TdConfig {
                games: train_games,
                alpha,
                max_turns: 30_000,
                seed: 0x5DEE_CE66 ^ generation.wrapping_mul(2_654_435_761) ^ i as u64,
                active: md.active.clone(),
                teams: md.teams,
                ..Default::default()
            };
            let cand = train_with(clone_model(&best[i]), &cfg);
            // Гейтинг (комбинированный, всё на ОДНИХ наборах бросков, усреднённо по сидам):
            //  1) кандидат бьёт лучшего ГОЛОВА-В-ГОЛОВУ с перевесом ≥ margin (это растёт и
            //     после того, как против эвристики уже потолок — линейка не насыщается);
            //  2) И не проседает против эвристики (cs ≥ bs − margin) — страхует от
            //     непереходности (A>B голова-в-голову, но в целом слабее).
            let eval_base = 1_000 + generation;
            let h2h = head_to_head_avg(&cand, &best[i], md, eval_games, eval_base, eval_seeds);
            let cs = strength_avg(&cand, md, eval_games, eval_base, eval_seeds);
            let bs = strength_avg(&best[i], md, eval_games, eval_base, eval_seeds);
            let bar = fair_share(md) + margin;
            let beats = h2h > bar;
            let no_regress = cs >= bs - margin;
            let mark = if beats && no_regress {
                best[i] = cand;
                save(&model_path(&prefix, md.tag), &best[i]);
                "ПРИНЯТА → сохранена"
            } else {
                "отклонена"
            };
            println!(
                "[generation {generation}] {:>8}: дуэль {:.1}% (нужно >{:.1}%), эвр {:.1}% vs {:.1}% — {mark}",
                md.name,
                100.0 * h2h,
                100.0 * bar,
                100.0 * cs,
                100.0 * bs
            );
            std::io::stdout().flush().ok();
        }
        generation += 1;
    }
}
