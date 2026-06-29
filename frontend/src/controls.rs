//! Переиспользуемые элементы управления настройками (тема / звук / скорость) — одни и
//! те же на экране меню, в партии и в обучении. Сигналы приходят сверху (общие на все
//! экраны), поэтому выбор сохраняется при переходах между ними.
use leptos::prelude::*;
use leptos_i18n::I18nContext;

use crate::i18n::*;
use crate::settings::{Speed, Theme};



/// Иконка-палитра с выпадающим списком тем.
#[component]
pub(crate) fn ThemeControl(
    theme: RwSignal<Theme>,
    menu_open: RwSignal<bool>,
    i18n: I18nContext<Locale>,
) -> impl IntoView {
    view! {
        <div class="theme-pick">
            <button class="icon-btn" title=move || t_string!(i18n, theme_title)
                on:click=move |_| menu_open.update(|o| *o = !*o)>"🎨"</button>
            {move || menu_open.get().then(|| view! {
                <div class="theme-menu">
                    {Theme::ALL.into_iter().map(|t| {
                        let label = move || match t {
                            Theme::Midnight => t_string!(i18n, theme_midnight),
                            Theme::Dusk => t_string!(i18n, theme_dusk),
                            Theme::Daylight => t_string!(i18n, theme_daylight),
                            Theme::Forest => t_string!(i18n, theme_forest),
                        };
                        view! {
                            <button class:on=move || theme.get() == t
                                on:click=move |_| { theme.set(t); menu_open.set(false); }>
                                {label}
                            </button>
                        }
                    }).collect_view()}
                </div>
            })}
        </div>
    }
}

/// Иконка-глобус с выпадающим списком языков.
#[component]
pub(crate) fn LocaleControl(
    i18n: I18nContext<Locale>,
    menu_open: RwSignal<bool>,
) -> impl IntoView {
    view! {
        <div class="locale-pick">
            <button class="icon-btn" title=move || t_string!(i18n, settings_lang)
                on:click=move |_| menu_open.update(|o| *o = !*o)>"🌐"</button>
            {move || menu_open.get().then(|| view! {
                <div class="locale-menu">
                    <button class:on=move || i18n.get_locale() == Locale::ru
                        on:click=move |_| {
                            let _ = web_sys::window().and_then(|w| w.local_storage().ok().flatten().map(|s| s.set_item("locale", "ru").ok()));
                            i18n.set_locale(Locale::ru);
                            menu_open.set(false);
                        }>"Русский"</button>
                    <button class:on=move || i18n.get_locale() == Locale::en
                        on:click=move |_| {
                            let _ = web_sys::window().and_then(|w| w.local_storage().ok().flatten().map(|s| s.set_item("locale", "en").ok()));
                            i18n.set_locale(Locale::en);
                            menu_open.set(false);
                        }>"English"</button>
                </div>
            })}
        </div>
    }
}

/// Ползунок скорости анимации (группа для всплывающей панели настроек).
#[component]
pub(crate) fn SpeedControl(speed: RwSignal<Speed>, i18n: I18nContext<Locale>) -> impl IntoView {
    view! {
        <div class="set-group">
            <span class="set-name">{move || t_string!(i18n, speed_name)}</span>
            <input type="range" class="speed-slider"
                min="0" max=move || (Speed::ALL.len() - 1).to_string() step="1"
                prop:value=move || speed.get().index().to_string()
                on:input=move |ev| {
                    let i: usize = event_target_value(&ev).parse().unwrap_or(2);
                    speed.set(Speed::from_index(i));
                } />
            <span class="speed-val">{move || match speed.get() {
                Speed::VerySlow => t_string!(i18n, speed_vslow),
                Speed::Slow => t_string!(i18n, speed_slow),
                Speed::Normal => t_string!(i18n, speed_normal),
                Speed::Fast => t_string!(i18n, speed_fast),
                Speed::VeryFast => t_string!(i18n, speed_vfast),
            }}</span>
        </div>
    }
}
