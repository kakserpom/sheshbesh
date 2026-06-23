//! Переиспользуемые элементы управления настройками (тема / звук / скорость) — одни и
//! те же на экране меню, в партии и в обучении. Сигналы приходят сверху (общие на все
//! экраны), поэтому выбор сохраняется при переходах между ними.
use leptos::prelude::*;

use crate::settings::{Speed, Theme};

/// Иконка-палитра с выпадающим списком тем. `menu_open` — общий сигнал открытия
/// списка (клик вне меню закрывает его в `App`).
#[component]
pub(crate) fn ThemeControl(theme: RwSignal<Theme>, menu_open: RwSignal<bool>) -> impl IntoView {
    view! {
        <div class="theme-pick">
            <button class="icon-btn" title="Тема оформления"
                on:click=move |_| menu_open.update(|o| *o = !*o)>"🎨"</button>
            {move || menu_open.get().then(|| view! {
                <div class="theme-menu">
                    {Theme::ALL.into_iter().map(|t| view! {
                        <button class:on=move || theme.get() == t
                            on:click=move |_| { theme.set(t); menu_open.set(false); }>
                            {t.label()}
                        </button>
                    }).collect_view()}
                </div>
            })}
        </div>
    }
}

/// Кнопка-тумблер звука.
#[component]
pub(crate) fn SoundControl(sound: RwSignal<bool>) -> impl IntoView {
    view! {
        <button class="icon-btn" class:on=move || sound.get()
            title=move || if sound.get() { "Звук включён" } else { "Звук выключен" }
            on:click=move |_| sound.update(|s| *s = !*s)>
            {move || if sound.get() { "🔊" } else { "🔇" }}
        </button>
    }
}

/// Ползунок скорости анимации (группа для всплывающей панели настроек).
#[component]
pub(crate) fn SpeedControl(speed: RwSignal<Speed>) -> impl IntoView {
    view! {
        <div class="set-group">
            <span class="set-name">"Скорость"</span>
            <input type="range" class="speed-slider"
                min="0" max=move || (Speed::ALL.len() - 1).to_string() step="1"
                prop:value=move || speed.get().index().to_string()
                on:input=move |ev| {
                    let i: usize = event_target_value(&ev).parse().unwrap_or(2);
                    speed.set(Speed::from_index(i));
                } />
            <span class="speed-val">{move || speed.get().label()}</span>
        </div>
    }
}

/// Иконка скорости «⏩» с выпадающим ползунком (drop-down под иконкой, как у темы).
/// `open` — общий сигнал открытия (Escape закрывает его в `App`). Общая для меню,
/// партии и обучения.
#[component]
pub(crate) fn SpeedMenu(open: RwSignal<bool>, speed: RwSignal<Speed>) -> impl IntoView {
    view! {
        <div class="theme-pick speed-pick">
            <button class="icon-btn" title="Скорость анимации"
                on:click=move |_| open.update(|o| *o = !*o)>"⏩"</button>
            {move || open.get().then(|| view! {
                <div class="theme-menu speed-menu">
                    <SpeedControl speed/>
                </div>
            })}
        </div>
    }
}
