//! Игровые настройки, меняемые на лету (без остановки партии): тема оформления,
//! скорость анимации и звук. Тема/скорость применяются к корню документа (CSS-
//! переменные + `data-theme`), звук синтезируется Web Audio API (без файлов).
//! Значения сохраняются в `localStorage`, чтобы переживать перезагрузку.
use std::cell::RefCell;
use wasm_bindgen::JsCast;
use web_sys::{AudioContext, BiquadFilterType, OscillatorType};

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
            Speed::VerySlow => 1.6,
            Speed::Slow => 1.0,
            Speed::Normal => 0.6,
            Speed::Fast => 0.32,
            Speed::VeryFast => 0.16,
        }
    }

    /// Длительность CSS-перехода позиции фишки (`--move-dur`).
    fn move_dur(self) -> &'static str {
        match self {
            Speed::VerySlow => "0.48s",
            Speed::Slow => "0.32s",
            Speed::Normal => "0.18s",
            Speed::Fast => "0.1s",
            Speed::VeryFast => "0.06s",
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
#[derive(Clone, Copy, PartialEq, Eq)]
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

impl SoundKind {
    /// Все звуки с подписями — для панели прослушивания в режиме разработчика.
    pub(crate) const ALL: [(SoundKind, &'static str); 9] = [
        (SoundKind::Dice, "🎲 Бросок"),
        (SoundKind::Enter, "⬇ Ввод"),
        (SoundKind::Capture, "💥 Съедание"),
        (SoundKind::PrisonIn, "🔒 В Тюрьму"),
        (SoundKind::PrisonOut, "🔓 Из Тюрьмы"),
        (SoundKind::Moon, "🌙 Луна"),
        (SoundKind::Ransom, "💰 Выкуп"),
        (SoundKind::Home, "🏠 Дом"),
        (SoundKind::Win, "🏆 Победа"),
    ];
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

/// Перкуссионная огибающая на параметре громкости: быстрая атака, плавный спад.
fn env(g: &web_sys::AudioParam, t0: f64, dur: f64, vol: f32, attack: f64) {
    let _ = g.set_value_at_time(0.0001, t0);
    let _ = g.linear_ramp_to_value_at_time(vol, t0 + attack);
    let _ = g.exponential_ramp_to_value_at_time(0.0001, t0 + dur);
}

/// Тон-«голос»: осциллятор → лоупас-фильтр → огибающая. Фильтр срезает верхние
/// гармоники (`cutoff`, Гц) — именно они делают меандр/пилу «жужжащими» и резкими,
/// так что лоупас заметно смягчает звук. `to` — конечная частота для глиссандо
/// (равна `freq`, если скольжения нет).
#[allow(clippy::too_many_arguments)]
fn voice(
    c: &AudioContext,
    freq: f64,
    to: f64,
    start: f64,
    dur: f64,
    wave: OscillatorType,
    vol: f32,
    cutoff: f64,
) {
    let (Ok(osc), Ok(gain), Ok(lp)) = (
        c.create_oscillator(),
        c.create_gain(),
        c.create_biquad_filter(),
    ) else {
        return;
    };
    osc.set_type(wave);
    let t0 = c.current_time() + start;
    let f = osc.frequency();
    let _ = f.set_value_at_time(freq as f32, t0);
    if (to - freq).abs() > f64::EPSILON {
        let _ = f.exponential_ramp_to_value_at_time(to as f32, t0 + dur);
    }
    lp.set_type(BiquadFilterType::Lowpass);
    lp.frequency().set_value(cutoff as f32);
    env(&gain.gain(), t0, dur, vol, 0.01);
    let _ = osc.connect_with_audio_node(&lp);
    let _ = lp.connect_with_audio_node(&gain);
    let _ = gain.connect_with_audio_node(&c.destination());
    let _ = osc.start_with_when(t0);
    let _ = osc.stop_with_when(t0 + dur + 0.03);
}

/// Шумовой щелчок: короткий буфер белого шума → полосовой фильтр (центр `center`,
/// добротность `q`) → огибающая. Звучит как удар/щелчок/стук — для костей и съедания,
/// где чистые тоны звучат искусственно.
fn noise(c: &AudioContext, start: f64, dur: f64, vol: f32, center: f64, q: f64) {
    let sr = c.sample_rate();
    let len = ((f64::from(sr) * dur) as u32).max(1);
    let Ok(buf) = c.create_buffer(1, len, sr) else {
        return;
    };
    let data: Vec<f32> = (0..len)
        .map(|_| (js_sys::Math::random() as f32) * 2.0 - 1.0)
        .collect();
    let _ = buf.copy_to_channel(&data, 0);
    let (Ok(src), Ok(bp), Ok(gain)) = (
        c.create_buffer_source(),
        c.create_biquad_filter(),
        c.create_gain(),
    ) else {
        return;
    };
    src.set_buffer(Some(&buf));
    bp.set_type(BiquadFilterType::Bandpass);
    bp.frequency().set_value(center as f32);
    bp.q().set_value(q as f32);
    let t0 = c.current_time() + start;
    env(&gain.gain(), t0, dur, vol, 0.004);
    let _ = src.connect_with_audio_node(&bp);
    let _ = bp.connect_with_audio_node(&gain);
    let _ = gain.connect_with_audio_node(&c.destination());
    let _ = src.start_with_when(t0);
}

/// Проигрывает короткий звук события (если звук включён — проверяет вызывающий).
pub(crate) fn play(kind: SoundKind) {
    let Some(c) = ctx() else { return };
    use OscillatorType::{Sine, Square, Triangle};
    match kind {
        // Кости стучат: три коротких шумовых щелчка чуть разной высоты — «трещотка».
        SoundKind::Dice => {
            noise(&c, 0.0, 0.05, 0.20, 2600.0, 1.2);
            noise(&c, 0.07, 0.05, 0.16, 2100.0, 1.2);
            noise(&c, 0.13, 0.06, 0.18, 3000.0, 1.0);
        }
        // Удар: глубокий тон со скольжением вниз + короткий шумовой щелчок-«шлепок».
        SoundKind::Capture => {
            voice(&c, 220.0, 70.0, 0.0, 0.22, Sine, 0.28, 1400.0);
            noise(&c, 0.0, 0.06, 0.16, 900.0, 0.8);
        }
        // В Тюрьму: тяжёлый «лязг» — два расстроенных тона скользят вниз под лоупасом.
        SoundKind::PrisonIn => {
            voice(&c, 200.0, 130.0, 0.0, 0.34, Triangle, 0.22, 800.0);
            voice(&c, 150.0, 98.0, 0.0, 0.34, Square, 0.07, 700.0);
        }
        // Из Тюрьмы: два светлых тона вверх — облегчение.
        SoundKind::PrisonOut => {
            voice(&c, 330.0, 330.0, 0.0, 0.13, Triangle, 0.16, 2600.0);
            voice(&c, 494.0, 494.0, 0.1, 0.18, Triangle, 0.16, 2800.0);
        }
        // Луна: мерцающее восходящее арпеджио чистыми синусами (с лёгкой расстройкой).
        SoundKind::Moon => {
            for (i, f) in [660.0, 988.0, 1319.0, 1760.0].into_iter().enumerate() {
                let t = i as f64 * 0.075;
                voice(&c, f, f, t, 0.18, Sine, 0.12, 6000.0);
                voice(&c, f * 1.003, f * 1.003, t, 0.18, Sine, 0.05, 6000.0);
            }
        }
        // Дом: тёплая восходящая терция (мягкий треугольник под лоупасом).
        SoundKind::Home => {
            voice(&c, 587.0, 587.0, 0.0, 0.16, Triangle, 0.18, 2400.0);
            voice(&c, 880.0, 880.0, 0.1, 0.26, Triangle, 0.18, 2600.0);
        }
        // Ввод фишки: короткий мягкий «бульк» вверх.
        SoundKind::Enter => voice(&c, 330.0, 440.0, 0.0, 0.12, Triangle, 0.16, 2200.0),
        // Выкуп: яркий «звон монеты» — два высоких блика.
        SoundKind::Ransom => {
            voice(&c, 988.0, 988.0, 0.0, 0.1, Triangle, 0.12, 4000.0);
            voice(&c, 1319.0, 1319.0, 0.07, 0.16, Triangle, 0.12, 4500.0);
        }
        // Победа: мажорное арпеджио C-E-G-C с длинной последней нотой (фанфара).
        SoundKind::Win => {
            for (i, f) in [523.0, 659.0, 784.0, 1047.0].into_iter().enumerate() {
                let last = i == 3;
                voice(
                    &c,
                    f,
                    f,
                    i as f64 * 0.12,
                    if last { 0.4 } else { 0.16 },
                    Triangle,
                    0.16,
                    3500.0,
                );
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
