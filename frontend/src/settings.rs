//! Игровые настройки, меняемые на лету (без остановки партии): тема оформления,
//! скорость анимации и звук. Тема/скорость применяются к корню документа (CSS-
//! переменные + `data-theme`), звук синтезируется Web Audio API (без файлов).
//! Значения сохраняются в `localStorage`, чтобы переживать перезагрузку.
use std::cell::RefCell;
use wasm_bindgen::JsCast;
use web_sys::{AudioContext, OscillatorType};

// --- Тема оформления ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Theme {
    Midnight,
    Daylight,
    Forest,
}

impl Theme {
    pub(crate) const ALL: [Theme; 3] = [Theme::Midnight, Theme::Daylight, Theme::Forest];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Theme::Midnight => "Полночь",
            Theme::Daylight => "Рассвет",
            Theme::Forest => "Лес",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Theme::Midnight => "midnight",
            Theme::Daylight => "daylight",
            Theme::Forest => "forest",
        }
    }

    fn from_key(s: &str) -> Theme {
        Theme::ALL
            .into_iter()
            .find(|t| t.key() == s)
            .unwrap_or(Theme::Midnight)
    }

    /// Следующая тема по кругу (для горячей клавиши).
    pub(crate) fn next(self) -> Theme {
        let i = Theme::ALL.iter().position(|&t| t == self).unwrap_or(0);
        Theme::ALL[(i + 1) % Theme::ALL.len()]
    }
}

// --- Скорость анимации -------------------------------------------------------

/// Скорость анимации — несколько дискретных положений ползунка (0 — медленнее).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Speed {
    VerySlow,
    Slow,
    Normal,
    Fast,
    VeryFast,
}

impl Speed {
    pub(crate) const ALL: [Speed; 5] = [
        Speed::VerySlow,
        Speed::Slow,
        Speed::Normal,
        Speed::Fast,
        Speed::VeryFast,
    ];

    /// Положение на ползунке (0..=4).
    pub(crate) fn index(self) -> usize {
        Speed::ALL.iter().position(|&s| s == self).unwrap_or(2)
    }

    pub(crate) fn from_index(i: usize) -> Speed {
        Speed::ALL.get(i).copied().unwrap_or(Speed::Normal)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Speed::VerySlow => "Очень медленно",
            Speed::Slow => "Медленно",
            Speed::Normal => "Обычная",
            Speed::Fast => "Быстро",
            Speed::VeryFast => "Очень быстро",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Speed::VerySlow => "vslow",
            Speed::Slow => "slow",
            Speed::Normal => "normal",
            Speed::Fast => "fast",
            Speed::VeryFast => "vfast",
        }
    }

    /// Множитель пауз между кадрами (1.0 — базовая скорость).
    pub(crate) fn factor(self) -> f64 {
        match self {
            Speed::VerySlow => 2.4,
            Speed::Slow => 1.6,
            Speed::Normal => 1.0,
            Speed::Fast => 0.6,
            Speed::VeryFast => 0.32,
        }
    }

    /// Длительность CSS-перехода позиции фишки (`--move-dur`).
    fn move_dur(self) -> &'static str {
        match self {
            Speed::VerySlow => "0.7s",
            Speed::Slow => "0.48s",
            Speed::Normal => "0.32s",
            Speed::Fast => "0.18s",
            Speed::VeryFast => "0.1s",
        }
    }

    fn from_key(s: &str) -> Speed {
        Speed::ALL
            .into_iter()
            .find(|t| t.key() == s)
            .unwrap_or(Speed::Normal)
    }
}

// --- Применение темы/скорости к корню документа ------------------------------

fn root() -> Option<web_sys::Element> {
    web_sys::window()?.document()?.document_element()
}

pub(crate) fn apply_theme(t: Theme) {
    if let Some(el) = root() {
        let _ = el.set_attribute("data-theme", t.key());
    }
}

pub(crate) fn apply_speed(s: Speed) {
    if let Some(html) = root().and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok()) {
        let _ = html.style().set_property("--move-dur", s.move_dur());
    }
}

// --- Сохранение в localStorage ----------------------------------------------

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// Загружает сохранённые настройки (тема, скорость, звук-вкл).
pub(crate) fn load() -> (Theme, Speed, bool) {
    let s = storage();
    let get = |k: &str| s.as_ref().and_then(|s| s.get_item(k).ok().flatten());
    let theme = get("theme").map_or(Theme::Midnight, |v| Theme::from_key(&v));
    let speed = get("speed").map_or(Speed::Normal, |v| Speed::from_key(&v));
    let sound = get("sound").is_none_or(|v| v == "on");
    (theme, speed, sound)
}

pub(crate) fn save_theme(t: Theme) {
    if let Some(s) = storage() {
        let _ = s.set_item("theme", t.key());
    }
}
pub(crate) fn save_speed(s: Speed) {
    if let Some(st) = storage() {
        let _ = st.set_item("speed", s.key());
    }
}
pub(crate) fn save_sound(on: bool) {
    if let Some(s) = storage() {
        let _ = s.set_item("sound", if on { "on" } else { "off" });
    }
}

// --- Звук (Web Audio) --------------------------------------------------------

/// Событие, под которое проигрывается короткий синтезированный звук.
#[derive(Clone, Copy)]
pub(crate) enum SoundKind {
    Dice,
    Capture,
    PrisonIn,
    PrisonOut,
    Moon,
    Home,
    Enter,
    Ransom,
    Win,
}

thread_local! {
    /// Единый AudioContext (создаётся лениво при первом звуке, после жеста пользователя).
    static AUDIO: RefCell<Option<AudioContext>> = const { RefCell::new(None) };
}

fn ctx() -> Option<AudioContext> {
    AUDIO.with(|a| {
        let mut a = a.borrow_mut();
        if a.is_none() {
            *a = AudioContext::new().ok();
        }
        if let Some(c) = a.as_ref() {
            let _ = c.resume(); // на случай suspended до жеста
        }
        a.clone()
    })
}

/// Один тон: частота, отступ от «сейчас», длительность, форма волны, громкость.
fn tone(c: &AudioContext, freq: f64, start: f64, dur: f64, wave: OscillatorType, vol: f32) {
    let (Ok(osc), Ok(gain)) = (c.create_oscillator(), c.create_gain()) else {
        return;
    };
    osc.set_type(wave);
    osc.frequency().set_value(freq as f32);
    let t0 = c.current_time() + start;
    let g = gain.gain();
    let _ = g.set_value_at_time(0.0001, t0);
    let _ = g.linear_ramp_to_value_at_time(vol, t0 + 0.012);
    let _ = g.exponential_ramp_to_value_at_time(0.0001, t0 + dur);
    let _ = osc.connect_with_audio_node(&gain);
    let _ = gain.connect_with_audio_node(&c.destination());
    let _ = osc.start_with_when(t0);
    let _ = osc.stop_with_when(t0 + dur + 0.03);
}

/// Проигрывает короткий звук события (если звук включён — проверяет вызывающий).
pub(crate) fn play(kind: SoundKind) {
    let Some(c) = ctx() else { return };
    use OscillatorType::{Sawtooth, Sine, Square, Triangle};
    match kind {
        SoundKind::Dice => {
            tone(&c, 230.0, 0.0, 0.05, Square, 0.07);
            tone(&c, 180.0, 0.06, 0.05, Square, 0.06);
            tone(&c, 280.0, 0.12, 0.05, Square, 0.06);
        }
        SoundKind::Capture => {
            tone(&c, 150.0, 0.0, 0.18, Sawtooth, 0.11);
            tone(&c, 75.0, 0.02, 0.22, Square, 0.1);
        }
        SoundKind::PrisonIn => tone(&c, 130.0, 0.0, 0.28, Square, 0.12),
        SoundKind::PrisonOut => {
            tone(&c, 300.0, 0.0, 0.12, Triangle, 0.1);
            tone(&c, 500.0, 0.1, 0.16, Triangle, 0.1);
        }
        SoundKind::Moon => {
            tone(&c, 600.0, 0.0, 0.1, Sine, 0.08);
            tone(&c, 900.0, 0.08, 0.12, Sine, 0.08);
            tone(&c, 1300.0, 0.16, 0.16, Sine, 0.07);
        }
        SoundKind::Home => {
            tone(&c, 660.0, 0.0, 0.12, Sine, 0.1);
            tone(&c, 990.0, 0.09, 0.2, Sine, 0.1);
        }
        SoundKind::Enter => tone(&c, 440.0, 0.0, 0.09, Triangle, 0.08),
        SoundKind::Ransom => {
            tone(&c, 820.0, 0.0, 0.09, Square, 0.07);
            tone(&c, 1100.0, 0.08, 0.13, Square, 0.07);
        }
        SoundKind::Win => {
            for (i, f) in [523.0, 659.0, 784.0, 1047.0].into_iter().enumerate() {
                tone(&c, f, i as f64 * 0.13, 0.22, Triangle, 0.11);
            }
        }
    }
}

/// Какому звуку соответствует реплика-комментарий кадра (`Frame.note`).
pub(crate) fn note_sound(note: &str) -> Option<SoundKind> {
    if note.contains("съедает") {
        Some(SoundKind::Capture)
    } else if note.contains("финиш") {
        Some(SoundKind::Win)
    } else if note.contains("в Тюрьму") {
        Some(SoundKind::PrisonIn)
    } else if note.contains("из Тюрьмы") {
        Some(SoundKind::PrisonOut)
    } else if note.contains("Луну") || note.contains("Луны") {
        Some(SoundKind::Moon)
    } else if note.contains("Дом") {
        Some(SoundKind::Home)
    } else if note.contains("выкуп") {
        Some(SoundKind::Ransom)
    } else if note.contains("ввод") {
        Some(SoundKind::Enter)
    } else {
        None
    }
}
