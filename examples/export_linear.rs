//! Обучает **линейные** ценности под все режимы GUI и сохраняет веса (little-endian
//! `f32`) для встраивания — отдельный алгоритм на выбор в настройках (наряду с MLP).
//! Запуск: `cargo run --release --example export_linear`
use sheshbesh::{Side, TdConfig, train};
use std::io::Write;

fn export(path: &str, active: Vec<Side>, teams: bool) {
    let model = train(&TdConfig {
        games: 18_000,
        max_turns: 30_000,
        active,
        teams,
        ..Default::default()
    });
    let floats = model.to_floats();
    let mut bytes = Vec::with_capacity(floats.len() * 4);
    for f in &floats {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    std::fs::File::create(path)
        .and_then(|mut f| f.write_all(&bytes))
        .expect("write model");
    eprintln!("{path}: {} floats ({} bytes)", floats.len(), bytes.len());
}

fn main() {
    use Side::{A, B, C, D};
    export("frontend/src/model_lin.bin", vec![A, C], false);
    export("frontend/src/model_lin_3p.bin", vec![A, B, C], false);
    export("frontend/src/model_lin_4p.bin", vec![A, B, C, D], false);
    export(
        "frontend/src/model_lin_4p_teams.bin",
        vec![A, B, C, D],
        true,
    );
}
