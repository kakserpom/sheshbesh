use crate::ai_worker::{init_worker, next_id, send_raw, state_json};
use crate::controls::{LocaleControl, SpeedControl, ThemeControl};
use crate::demo::*;
use crate::geom::*;
use crate::i18n::*;
use crate::lessons::*;
use crate::model::*;
use crate::moves_ui::*;
use crate::settings::{self, Speed, Theme};
use crate::util::*;
use crate::view::*;
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use leptos_meta::Title;
use sheshbesh::board::{LOCAL_MOON, LOCAL_PRISON_NEAR, SIDE_LEN};
use sheshbesh::moves::{forced_six_moves, move_legal};
use sheshbesh::{
    Agent, BOARD_DIM, BOARD_MARGIN, DiceRoll, DiceSource, Die, Game, GameState,
    Heuristic, MoonField, Move, MoveKind, Position, RandomDice, Side, Value, apply,
    best_forced, best_turn, legal_turns, legal_turns_remaining, margin_coord, remaining_after,
};
use sheshbesh::turn::next_unfinished_active;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

#[allow(non_snake_case)]
pub(crate) fn App() -> impl IntoView {
    view! {
        <I18nContextProvider>
            <GameApp />
        </I18nContextProvider>
    }
}

#[component]
fn GameApp() -> impl IntoView {
    let i18n = use_i18n();
    // Restore locale from localStorage on startup; fall back to system language.
    if let Some(window) = web_sys::window() {
        if let Ok(Some(s)) = window.local_storage() {
            let saved = s.get_item("locale").ok().flatten();
            let locale = match saved.as_deref() {
                Some("en") => Locale::en,
                Some("ru") => Locale::ru,
                // First run — detect browser language
                _ => {
                    let lang = window.navigator().language().unwrap_or_default();
                    let detected = if lang.starts_with("ru") { Locale::ru } else { Locale::en };
                    // Persist detected locale
                    let _ = s.set_item("locale", if detected == Locale::en { "en" } else { "ru" });
                    detected
                }
            };
            i18n.set_locale(locale);
        }
    }
    init_worker();
    let dice = StoredValue::new(RandomDice::from_seed(seed()));
    // Поколение партии: растёт при старте/остановке игры. Любая запущенная анимация
    // (spawn_local) запоминает своё поколение и прекращается, когда оно устарело, —
    // иначе старая партия продолжала бы крутить кадры поверх новой.
    let epoch = StoredValue::new(0u64);
    // Настройки: число игроков, типы сторон и (для 4) командный режим 2×2.
    let players = RwSignal::new(2usize);
    let humans = RwSignal::new(vec![Side::A]);
    let teams = RwSignal::new(false);
    // Алгоритм компьютера для каждой стороны (по индексу `Side`); по умолчанию MLP.
    let algos = RwSignal::new([Algo::Mlp; 4]);
    let started = RwSignal::new(false);
    // Режим разработчика: экран демонстрации состояний и анимаций (вне партии).
    let dev = RwSignal::new(false);
    // Справка по правилам и пошаговое обучение (туториал) — экраны из меню настроек.
    let rules = RwSignal::new(false);
    let about = RwSignal::new(false);
    let tutorial = RwSignal::new(false);
    let lesson_idx = RwSignal::new(0usize);
    let lessons_sv = StoredValue::new(lessons(i18n));
    // Выбрана ли демонстрационная фишка (первый клик хода в туториале).
    let tut_sel = RwSignal::new(false);
    // Сколько под-ходов текущего шага сыграно (ход = последовательность по костям;
    // в туториале игрок проигрывает их по одному — как в реальной игре). Для шага
    // выкупа: 0 — выкуп ещё не сыгран, 1 — выбираем обязательный ход на 6.
    let tut_played = RwSignal::new(0usize);
    // Выбранная фишка для обязательного хода на 6 (шаг выкупа).
    let tut_pick = RwSignal::new(None::<usize>);
    // Стороны, финишировавшие (все фишки в Доме) — в порядке финиша.
    let finished = RwSignal::new(Vec::<Side>::new());
    // Пауза: блокирует автоход ИИ и авто-бросок; анимация замирает между кадрами.
    let paused = RwSignal::new(false);
    let game = RwSignal::new(fresh(dice, active_for(2), false));
    let roll = RwSignal::new(None::<DiceRoll>);
    let turns = RwSignal::new(Vec::<Vec<Move>>::new());
    let prefix = RwSignal::new(Vec::<Move>::new());
    let sel = RwSignal::new(None::<Sel>);
    // Оставшиеся кости текущего хода человека (по-шагово). После выкупа человеком
    // обязательный ответ захватчика применяется сразу, остаток костей пересчитывается,
    // а решение об остатке принимается уже после ответа.
    let remaining = RwSignal::new(Vec::<u8>::new());
    // Ожидание ОБЯЗАТЕЛЬНОГО ответа на «6» от ЧЕЛОВЕКА-захватчика, когда выкуп сделал
    // компьютер: `Some((ходящая_сторона, захватчик, бросок, остаток_костей))`. Пока он
    // стоит, автоход компьютера приостановлен — человек сам выбирает обязательный ход;
    // после выбора компьютер ПРОДОЛЖАЕТ ход остатком костей (по-шагово).
    let forced_pick = RwSignal::new(None::<(Side, Side, DiceRoll, Vec<u8>)>);
    // Состояние на начало текущего хода человека (нужно для доигровки конца хода:
    // вынужденного ответа выкупа и передачи очереди), пока доска уже продвинута.
    let turn_start = StoredValue::new(game.get_untracked().state.clone());
    // Фактически применённые ходы текущего хода КОМПЬЮТЕРА (ход игрока + ответы
    // захватчика при выкупе) — копятся по шагам и пишутся в отладочный лог одной
    // строкой по завершении хода (ход компьютера строится вручную, минуя `commit_turn`,
    // поэтому `dbg_log_turn` его не покрывает). Сбрасывается в начале каждого хода ИИ.
    let comp_applied = StoredValue::new(Vec::<Move>::new());
    // Идёт ли сейчас проигрывание анимации хода (блокирует ввод и авто-бросок).
    let animating = RwSignal::new(false);
    // Крутятся ли сейчас кости (анимация броска).
    let rolling = RwSignal::new(false);
    // Покадровое переопределение точек фишек (для движения по параболе Луны).
    let anim_pts = RwSignal::new(Vec::<(usize, f64, f64)>::new());
    // Гашение доски (fade) на кадрах-«телепортах»: смена расстановки шага обучения
    // без «перелёта» фишек через всю доску.
    let fading = RwSignal::new(false);
    // Текст ведущего-комментатора над доской.
    let herald = RwSignal::new(String::new());
    // Показан ли технический лог партии (кнопка в отладочной сборке).
    let show_log = RwSignal::new(false);
    // Показана ли подсказка с горячими клавишами хода (кнопка «⌨» в партии).
    let keys_hint = RwSignal::new(false);

    // Настройки, меняемые на лету (тема/скорость/звук) — грузим сохранённые.
    let (init_theme, init_speed, init_sound) = settings::load();
    let theme = RwSignal::new(init_theme);
    let speed = RwSignal::new(init_speed);
    let sound = RwSignal::new(init_sound);
    // Мобильное меню (гамбургер).
    let hamburger = RwSignal::new(false);
    // Открыт ли выпадающий ползунок скорости (по иконке «⏩»).
    let speed_menu = RwSignal::new(false);
    // Открыт ли выпадающий список выбора языка (по иконке 🌐).
    let locale_menu = RwSignal::new(false);
    // Открыт ли выпадающий список выбора темы (в основном меню, по иконке-палитре).
    let theme_menu = RwSignal::new(false);
    // Применяем тему/скорость к корню документа и сохраняем при каждом изменении.
    Effect::new(move |_| {
        let t = theme.get();
        settings::apply_theme(t);
        settings::save_theme(t);
    });
    Effect::new(move |_| {
        let s = speed.get();
        settings::apply_speed(s);
        settings::save_speed(s);
    });
    Effect::new(move |_| settings::save_sound(sound.get()));

    // Горячие клавиши. Escape закрывает любые всплывающие окна; s/t/p (по физической
    // клавише, не зависит от раскладки) — звук / следующая тема / пауза (пауза — только
    // в партии).
    window_event_listener(leptos::ev::keydown, move |e| {
        if e.key() == "Escape" {
            // Escape действует послойно: если открыто всплывающее окно (лог, ползунок
            // скорости, меню темы, Правила, «От автора») — закрываем ТОЛЬКО его. Если
            // ничего не открыто, а мы в обучении — выходим из обучения в меню (как «←»).
            // Так из открытых в обучении Правил первый Escape вернёт к обучению, а второй
            // — в главное меню.
            let popup_open = show_log.get_untracked()
                || keys_hint.get_untracked()
                || speed_menu.get_untracked()
                || theme_menu.get_untracked()
                || locale_menu.get_untracked()
                || hamburger.get_untracked()
                || rules.get_untracked()
                || about.get_untracked();
            if popup_open {
                show_log.set(false);
                keys_hint.set(false);
                speed_menu.set(false);
                theme_menu.set(false);
                locale_menu.set(false);
                hamburger.set(false);
                rules.set(false);
                about.set(false);
            } else if tutorial.get_untracked() {
                epoch.update_value(|n| *n += 1);
                animating.set(false);
                rolling.set(false);
                tutorial.set(false);
            }
            return;
        }
        // Не перехватываем буквы, когда фокус в поле ввода (textarea лога, ползунок, select).
        let in_field = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|el| matches!(el.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT"));
        if in_field {
            return;
        }
        match e.code().as_str() {
            "KeyS" => sound.update(|s| *s = !*s),
            "KeyT" => theme.update(|t| *t = t.next()),
            // Пауза — только в партии (не в обучении/режиме разработчика).
            "KeyP" if started.get_untracked() && !tutorial.get_untracked() && !dev.get_untracked() => {
                paused.update(|p| *p = !*p);
            }
            _ => {}
        }
    });

    // Клик вне выпадающих настроек закрывает их. Чтобы переключатели продолжали
    // работать, клик ВНУТРИ своего wrapper'а (`.theme-pick`/`.speed-pick`) меню не
    // трогает (его переключит собственный обработчик), но закрывает СОСЕДНЕЕ меню;
    // клик вне обоих — закрывает оба. (Эмиттится после on:click кнопки — всплытие.)
    window_event_listener(leptos::ev::click, move |e| {
        // Разбудить AudioContext по жесту пользователя (iOS без этого не играет).
        settings::warm_audio();
        let el = e.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok());
        let inside = |sel: &str| {
            el.as_ref()
                .is_some_and(|el| el.closest(sel).ok().flatten().is_some())
        };
        let outside_hamburger = !el.as_ref().is_some_and(|el| el.closest(".hamburger-btn, .hamburger-content.open").ok().flatten().is_some());
        if outside_hamburger && hamburger.get_untracked() {
            hamburger.set(false);
        }
        let (close_theme, close_speed, close_locale) = if inside(".speed-pick, .hamburger-speed-menu") {
            (true, false, true)
        } else if inside(".theme-pick, .hamburger-theme-menu") {
            (false, true, true)
        } else if inside(".locale-pick, .locale-menu, .hamburger-locale-btn") {
            (true, true, false)
        } else {
            (true, true, true)
        };
        if close_theme && theme_menu.get_untracked() {
            theme_menu.set(false);
        }
        if close_speed && speed_menu.get_untracked() {
            speed_menu.set(false);
        }
        if close_locale && locale_menu.get_untracked() {
            locale_menu.set(false);
        }
    });

    // Следим за финишами: запоминаем порядок финиша, а по окончании партии (с учётом
    // режима) выводим итоговую реплику.
    Effect::new(move |_| {
        let g = game.get();
        // В режиме разработчика и обучении доска несёт демо-состояния — не финиши.
        if dev.get_untracked() || tutorial.get_untracked() {
            return;
        }
        let mut newly = Vec::new();
        finished.with_untracked(|f| {
            for &s in &g.state.active {
                if g.state.has_won(s) && !f.contains(&s) {
                    newly.push(s);
                }
            }
        });
        if !newly.is_empty() {
            finished.update(|f| f.extend(newly));
        }
        if let Some(msg) = result_msg(
            &g.state,
            &finished.get_untracked(),
            teams.get_untracked(),
            &humans.get_untracked(),
            i18n,
        ) {
            herald.set(msg);
        }
    });

    // Проигрывает кадры с паузами, обновляя доску и кости; в конце снимает блокировку.
    // На паузе замирает между кадрами (опрос `paused`).
    let play = move |frames: Vec<Frame>| {
        if frames.is_empty() {
            return;
        }
        let era = epoch.get_value();
        animating.set(true);
        fading.set(false);
        spawn_local(async move {
            for frame in frames {
                // Новая партия началась — бросаем эту анимацию (не трогая сигналы).
                if epoch.get_value() != era {
                    return;
                }
                while paused.get_untracked() {
                    if epoch.get_value() != era {
                        return;
                    }
                    TimeoutFuture::new(150).await;
                }
                // Кадр-«телепорт»: гасим доску, ставим новую расстановку «вслепую»
                // (CSS подавляет переход позиций, пока доска прозрачна), затем проявляем —
                // фишки не «перелетают» через всю доску.
                if frame.fade {
                    fading.set(true);
                    TimeoutFuture::new(FADE_MS).await;
                    if epoch.get_value() != era {
                        return;
                    }
                }
                // На максимальной скорости бросок без анимации — кости появляются
                // мгновенно (не крутим кубик, кадр «кости крутятся» держим минимум).
                let instant_roll = frame.rolling && speed.get_untracked() == Speed::VeryFast;
                // Показываем кадр, затем держим его свою паузу (бросок — дольше шага).
                game.update(|g| g.state = frame.state);
                roll.set(frame.roll);
                rolling.set(frame.rolling && !instant_roll);
                anim_pts.set(frame.pts);
                // Звук: бросок костей — на кадре «кости крутятся», событийные звуки —
                // по реплике-комментарию кадра (если звук включён в настройках).
                if sound.get_untracked() {
                    if frame.rolling {
                        settings::play(settings::SoundKind::Dice);
                    }
                    if let Some(k) = frame.sound {
                        settings::play(k);
                    }
                }
                if let Some(note) = frame.note {
                    herald.set(note);
                }
                if frame.fade {
                    // Дать расстановке встать «вслепую», затем проявить доску.
                    TimeoutFuture::new(FADE_MS).await;
                    fading.set(false);
                }
                // Пауза кадра масштабируется выбранной скоростью (меняется на лету);
                // мгновенный бросок держим лишь чуть-чуть.
                let hold = if instant_roll {
                    70
                } else {
                    (f64::from(frame.hold) * speed.get_untracked().factor()) as u32
                };
                TimeoutFuture::new(hold).await;
            }
            if epoch.get_value() != era {
                return;
            }
            anim_pts.set(Vec::new());
            rolling.set(false);
            animating.set(false);
            fading.set(false);
        });
    };

    // По-шаговый ход КОМПЬЮТЕРА: продолжает заполнять `frames` от состояния `st` с
    // остатком костей `rem`. После выкупа СРАЗУ применяется обязательный ответ
    // захватчика; если захватчик — ЧЕЛОВЕК, ставим `forced_pick` и приостанавливаемся
    // (человек выберет ответ, затем `forced_apply` продолжит ЭТОТ ЖЕ ход остатком костей).
    // Иначе компьютер выбирает ответ сам и идёт дальше — решение об остатке принимается
    // уже ПОСЛЕ ответа (как и требует правило).
    let play_computer = move |mut frames: Vec<Frame>, mut st: GameState, mut rem: Vec<u8>, roll: DiceRoll, first_idx: Option<usize>| {
        let side = st.to_move;
        let hs = humans.get_untracked();
        let algo = algos.get_untracked()[side.index()];
        let m = ai_model_for(algo, st.active.len(), teams.get_untracked());
        let mut first_used = first_idx.is_none();
        loop {
            let turns = legal_turns_remaining(&st, &rem);
            if turns.iter().all(Vec::is_empty) {
                break;
            }
            let idx = if first_used || !algo.is_search() {
                best_turn(&m, &st, &turns)
            } else {
                first_used = true;
                first_idx.unwrap()
            }
            .min(turns.len() - 1);
            let mut interrupted = false;
            for &mv in &turns[idx] {
                let captor = if mv.kind == MoveKind::Ransom {
                    match st.checkers[mv.checker].pos {
                        Position::Captured { captor } => Some(captor),
                        _ => None,
                    }
                } else {
                    None
                };
                st = apply_with_frames(&mut frames, st, mv, roll, &hs, i18n);
                rem = remaining_after(&rem, mv.pips);
                comp_applied.update_value(|v| v.push(mv));
                if let Some(captor) = captor {
                    let options = forced_six_moves(&st, captor);
                    if !options.is_empty() {
                        if hs.contains(&captor) {
                            if let Some(last) = frames.last_mut() {
                                last.note = Some(tu_string!(i18n, forced_pick_msg, who = side_name(captor, &hs, i18n)).to_string());
                            }
                            // Подсветка по умолчанию для выбора с клавиатуры (Tab/Enter):
                            // первый вариант обязательного хода.
                            sel.set(Some(move_source(&st, options[0])));
                            forced_pick.set(Some((side, captor, roll, rem.clone())));
                            play(frames);
                            return;
                        }
                        let fidx = best_forced(&m, &st, captor, &options).min(options.len() - 1);
                        st = apply_with_frames(&mut frames, st, options[fidx], roll, &hs, i18n);
                        comp_applied.update_value(|v| v.push(options[fidx]));
                        if let Some(last) = frames.last_mut() {
                            last.note = Some(tu_string!(i18n, forced_reply, who = side_name(captor, &hs, i18n)).to_string());
                        }
                    }
                    interrupted = true;
                    break; // переспросить остаток уже от нового состояния
                }
            }
            if !interrupted || rem.is_empty() {
                break;
            }
        }
        // Ход компьютера завершён — пишем его в отладочный лог одной строкой (как ход
        // человека через `dbg_log_turn`). На дубль придётся новый ход (новый бросок),
        // он залогируется отдельной строкой.
        if cfg!(debug_assertions) {
            dbg_log_moves(side, roll, &comp_applied.get_value());
        }
        let again = roll.is_double() && !st.has_won(side);
        if !again {
            st.to_move = next_unfinished_active(&st, side);
        }
        frames.push(Frame {
            state: st,
            roll: None,
            hold: HOLD_STEP_MS,
            rolling: false,
            note: None,
            pts: Vec::new(),
            fade: false,
            sound: None,
        });
        play(frames);
    };

    // Автоход компьютерных сторон: бросок + по-шаговый ход (`play_computer`). Цепочка
    // ходов «сама себя» продолжает через `animating` (после анимации он снимается →
    // Effect срабатывает снова). Пауза/победа/ход человека/ожидание выкупа её прерывают.
    // Кости показываются НЕМЕДЛЕННО (до вычислений), чтобы кубик не зависал на время
    // тяжёлого 2-плай поиска на WASM; между броском и расчётом — точка yield.
    Effect::new(move |_| {
        let g = game.get();
        let hs = humans.get_untracked();
        if !started.get()
            || animating.get()
            || paused.get()
            || rules.get()
            || forced_pick.get().is_some() // ждём обязательный ответ человека на выкуп
            || game_over(&g.state, teams.get_untracked())
            || hs.contains(&g.state.to_move)
        {
            return;
        }
        let mut roll_v = None;
        dice.update_value(|d| roll_v = Some(d.roll()));
        let r = roll_v.expect("roll");
        let side = g.state.to_move;
        let no_move = legal_turns(&g.state, r).iter().all(Vec::is_empty);
        let era = epoch.get_value();
        let factor = speed.get_untracked().factor();
        let instant = speed.get_untracked() == Speed::VeryFast;
        let roll_ms = if instant { 70 } else { (f64::from(ROLL_ANIM_MS) * factor) as u32 };
        let name = side_name(side, &hs, i18n);
        let [a, b] = r.values();
        // Крутим кубик немедленно — до того, как начнётся расчёт хода ИИ.
        roll.set(Some(r));
        rolling.set(!instant);
        herald.set(tu_string!(i18n, herald_roll, who = &name).to_string());
        if sound.get_untracked() {
            settings::play(settings::SoundKind::Dice);
        }
        spawn_local(async move {
            animating.set(true);
            TimeoutFuture::new(roll_ms).await;
            if epoch.get_value() != era {
                animating.set(false);
                return;
            }
            rolling.set(false);
            // Показываем результат броска — до расчёта хода ИИ и анимации (иначе
            // herald перескочил бы с «AI rolls...» сразу на сообщение следующего хода).
            if !no_move {
                herald.set(tu_string!(i18n, herald_ai_thinking, who = &name, a = a, b = b).to_string());
            }
            // Пауза — не начинаем расчёт, пока пользователь не снимет.
            while paused.get_untracked() {
                TimeoutFuture::new(150).await;
                if epoch.get_value() != era {
                    animating.set(false);
                    return;
                }
            }
            if no_move {
                herald.set(tu_string!(i18n, herald_no_move, who = &name, a = a, b = b).to_string());
                let nomove_ms = (f64::from(HOLD_NOMOVE_MS) * factor) as u32;
                TimeoutFuture::new(nomove_ms).await;
                if epoch.get_value() != era {
                    animating.set(false);
                    return;
                }
                let mut fg = Game::new(g.state.clone());
                fg.commit_turn(r, Vec::new(), |_, _, _| 0);
                play(vec![Frame {
                    state: fg.state,
                    roll: None,
                    hold: HOLD_STEP_MS,
                    rolling: false,
                    note: None,
                    pts: Vec::new(),
                    fade: false,
                    sound: None,
                }]);
                return;
            }
            comp_applied.set_value(Vec::new());
            let frames = Vec::new();
            let st = g.state.clone();
            let rem = r.values().to_vec();
            let algo = algos.get_untracked()[side.index()];
            if algo.is_search() {
                let id = next_id();
                let sj = state_json(&st);
                let rem_json = rem.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",");
                let ri = r.values();
                let msg = format!(
                    r#"{{"id":{},"mode":"turn","state":{},"roll":[{},{}],"rem":[{}],"algo":1}}"#,
                    id, sj, ri[0], ri[1], rem_json
                );
                let signal = send_raw(&msg, id);
                let first_idx = loop {
                    if epoch.get_value() != era {
                        animating.set(false);
                        return;
                    }
                    if let Some(resp) = signal.get_untracked() {
                        break resp;
                    }
                    TimeoutFuture::new(20).await;
                };
                if let Some(idx) = first_idx {
                    play_computer(frames, st, rem, r, Some(idx));
                } else {
                    herald.set(tu_string!(i18n, herald_no_move, who = &name, a = a, b = b).to_string());
                    let nomove_ms = (f64::from(HOLD_NOMOVE_MS) * factor) as u32;
                    TimeoutFuture::new(nomove_ms).await;
                    if epoch.get_value() != era {
                        animating.set(false);
                        return;
                    }
                    let mut fg = Game::new(g.state.clone());
                    fg.commit_turn(r, Vec::new(), |_, _, _| 0);
                    play(vec![Frame {
                        state: fg.state,
                        roll: None,
                        hold: HOLD_STEP_MS,
                        rolling: false,
                        note: None,
                        pts: Vec::new(),
                        fade: false,
                        sound: None,
                    }]);
                }
            } else {
                play_computer(frames, st, rem, r, None);
            }
        });
    });

    // Старт партии: держим паузу-заставку (видно «Игра началась»), затем снимаем
    // блокировку — дальше ход подхватят Effect'ы (авто-бросок человека / ход ИИ).
    let kickoff = move || {
        let era = epoch.get_value();
        animating.set(true);
        spawn_local(async move {
            TimeoutFuture::new(INTRO_MS).await;
            if epoch.get_value() != era {
                return;
            }
            animating.set(false);
        });
    };

    // Авто-бросок в начале хода игрока-человека (но не во время анимации).
    Effect::new(move |_| {
        let g = game.get();
        let busy = animating.get();
        let pause = paused.get() || rules.get();
        let hs = humans.get_untracked();
        if started.get()
            && !busy
            && !pause
            && !game_over(&g.state, teams.get_untracked())
            && hs.contains(&g.state.to_move)
            && roll.get_untracked().is_none()
        {
            let mut r = None;
            dice.update_value(|d| r = Some(d.roll()));
            let r = r.expect("roll");
            turn_start.set_value(g.state.clone());
            let t = legal_turns(&g.state, r);
            let no_move = t.first().is_none_or(Vec::is_empty);
            let [a, b] = r.values();
            let name = side_name(g.state.to_move, &hs, i18n);
            // Анимируем бросок игрока так же, как у компьютера: кубики кувыркаются,
            // затем показывается результат и включается выбор хода.
            let era = epoch.get_value();
            animating.set(true);
            // Бросок игрока уважает выбранную скорость (как у компьютера): паузы
            // масштабируются `factor`, а на максимальной скорости бросок мгновенный
            // (кубик не кувыркается).
            let factor = speed.get_untracked().factor();
            let instant = speed.get_untracked() == Speed::VeryFast;
            let roll_ms = if instant {
                70
            } else {
                (f64::from(ROLL_ANIM_MS) * factor) as u32
            };
            let nomove_ms = (f64::from(HOLD_NOMOVE_MS) * factor) as u32;
            spawn_local(async move {
                herald.set(tu_string!(i18n, herald_roll, who = &name).to_string());
                roll.set(Some(r));
                rolling.set(!instant);
                TimeoutFuture::new(roll_ms).await;
                // Новая партия началась за время броска — прекращаем.
                if epoch.get_value() != era {
                    return;
                }
                rolling.set(false);
                if no_move {
                    // Ходить нечем: показываем бросок и САМИ пропускаем ход через паузу
                    // (без кнопки) — как это делает компьютер.
                    herald.set(tu_string!(i18n, herald_no_move, who = &name, a = a, b = b).to_string());
                    TimeoutFuture::new(nomove_ms).await;
                    if epoch.get_value() != era {
                        return;
                    }
                    let mut fg = Game::new(turn_start.get_value());
                    fg.commit_turn(r, Vec::new(), |_, _, _| 0);
                    turns.set(Vec::new());
                    prefix.set(Vec::new());
                    sel.set(None);
                    // `play` сам снимет `animating` по завершении кадра передачи хода.
                    play(vec![Frame {
                        state: fg.state,
                        roll: None,
                        hold: HOLD_STEP_MS,
                        rolling: false,
                        note: None,
                        pts: Vec::new(),
                        fade: false,
                        sound: None,
                    }]);
                    return;
                }
                herald.set(if r.is_double() {
                    tu_string!(i18n, herald_double, who = &name, a = a, b = b).to_string()
                } else {
                    tu_string!(i18n, herald_wait_move, who = &name, a = a, b = b).to_string()
                });
                turns.set(t);
                prefix.set(Vec::new());
                sel.set(None);
                remaining.set(r.values().to_vec());
                animating.set(false);
            });
        }
    });

    // Доигровка хода человека: применённые ходы (`played`) дают `after`; отсюда
    // доигрываем вынужденный ответ выкупа и фиксируем передачу очереди. Дальнейшие
    // ходы компьютера подхватит Effect автоматического хода.
    let finish = move |mut frames: Vec<Frame>, after: GameState, played: Vec<Move>| {
        let r = roll.get_untracked().expect("roll");
        let hs = humans.get_untracked();
        let mut fg = Game::new(turn_start.get_value());
        let outcome = fg.commit_turn(r, played, |_, _, _| 0);
        if cfg!(debug_assertions) {
            dbg_log_turn(&outcome);
        }
        let mut scratch = after;
        for &fm in &outcome.forced {
            scratch = apply_with_frames(&mut frames, scratch, fm, r, &hs, i18n);
        }
        let _ = scratch;
        // Если использованы не все очки броска — остаток сыграть нечем, он «сгорает».
        // Берём фактический остаток костей хода (`remaining`): после выкупа человеком
        // обязательный ответ и пересчёт меняют картину, и по `played` судить нельзя.
        let rem = remaining.get_untracked();
        let burn_note = (!rem.is_empty()).then(|| {
            let burned: u8 = rem.iter().sum();
            let name = side_name(outcome.side, &hs, i18n);
            if outcome.again {
                tu_string!(i18n, burn_double, who = &name, n = burned as u16).to_string()
            } else {
                tu_string!(i18n, burn_pip, who = &name, n = burned as u16).to_string()
            }
        });
        frames.push(Frame {
            state: fg.state.clone(),
            roll: None,
            hold: if burn_note.is_some() {
                HOLD_NOMOVE_MS
            } else {
                HOLD_STEP_MS
            },
            rolling: false,
            note: burn_note,
            pts: Vec::new(),
            fade: false,
            sound: None,
        });
        turns.set(Vec::new());
        prefix.set(Vec::new());
        sel.set(None);
        remaining.set(Vec::new());
        play(frames);
    };

    // Играет ОДНУ часть хода `m` (как клик по её цели): применяет её, при выкупе доигрывает
    // обязательный ответ захватчика, и либо завершает ход, либо оставляет фокус на фишке.
    // Общая для кликов и горячих клавиш; сама проверяет, что сейчас ход человека.
    let play_part = move |m: Move| {
        let g = game.get_untracked();
        let hs = humans.get_untracked();
        if animating.get_untracked()
            || paused.get_untracked()
            || game_over(&g.state, teams.get_untracked())
            || !hs.contains(&g.state.to_move)
            || roll.get_untracked().is_none()
        {
            return;
        }
        let r = roll.get_untracked().expect("roll");
        let ps = g.state.clone();
        let pre = prefix.get_untracked();
        let mut frames = Vec::new();
        let after = apply_with_frames(&mut frames, ps.clone(), m, r, &hs, i18n);
        remaining.update(|rem| *rem = remaining_after(rem, m.pips));
        if m.kind == MoveKind::Ransom {
            let captor = match ps.checkers[m.checker].pos {
                Position::Captured { captor } => captor,
                _ => return,
            };
            let mut after = after;
            let options = forced_six_moves(&after, captor);
            if !options.is_empty() {
                let cm = ai_model_for(
                    algos.get_untracked()[captor.index()],
                    after.active.len(),
                    teams.get_untracked(),
                );
                let fidx = best_forced(&cm, &after, captor, &options).min(options.len() - 1);
                after = apply_with_frames(&mut frames, after, options[fidx], r, &hs, i18n);
                if let Some(last) = frames.last_mut() {
                    last.note = Some(tu_string!(i18n, forced_reply, who = side_name(captor, &hs, i18n)).to_string());
                }
            }
            turn_start.set_value(after.clone());
            let rem = remaining.get_untracked();
            let new_turns = legal_turns_remaining(&after, &rem);
            prefix.set(Vec::new());
            sel.set(None);
            if new_turns.iter().all(Vec::is_empty) {
                finish(frames, after, Vec::new());
            } else {
                turns.set(new_turns);
                play(frames);
            }
            return;
        }
        let mut np = pre.clone();
        np.push(m);
        let next_steps = step_opts(&turns.get_untracked(), &np);
        if next_steps.is_empty() {
            finish(frames, after, np);
        } else {
            let next_src = sel_of(after.checkers[m.checker].owner, after.checkers[m.checker].pos);
            let keep = next_steps.iter().any(|&mv| move_source(&after, mv) == next_src);
            prefix.set(np);
            sel.set(keep.then_some(next_src));
            play(frames);
        }
    };

    // Играет всю последовательность `seq` одной фишкой («сразу за обе кости») и завершает
    // ход. Без выкупов (`combo_targets` их исключает).
    let play_full = move |seq: Vec<Move>| {
        let g = game.get_untracked();
        let hs = humans.get_untracked();
        if seq.is_empty()
            || animating.get_untracked()
            || paused.get_untracked()
            || game_over(&g.state, teams.get_untracked())
            || !hs.contains(&g.state.to_move)
            || roll.get_untracked().is_none()
        {
            return;
        }
        let r = roll.get_untracked().expect("roll");
        let pre = prefix.get_untracked();
        let mut frames = Vec::new();
        let mut st = g.state.clone();
        for &mv in &seq {
            st = apply_with_frames(&mut frames, st, mv, r, &hs, i18n);
            remaining.update(|rem| *rem = remaining_after(rem, mv.pips));
        }
        let mut played = pre.clone();
        played.extend(seq);
        finish(frames, st, played);
    };

    // Применение выбранного ЧЕЛОВЕКОМ обязательного ответа на «6» (выкуп сделал
    // компьютер): проигрываем ответ, снимаем `forced_pick` и ПРОДОЛЖАЕМ ход компьютера
    // остатком костей (`play_computer`) — теперь компьютер решает остаток уже после ответа.
    let forced_apply = move |fm: Move| {
        if animating.get_untracked() {
            return;
        }
        let Some((_side, _captor, roll, rem)) = forced_pick.get_untracked() else {
            return;
        };
        let hs = humans.get_untracked();
        let st = game.get_untracked().state;
        let mut frames = Vec::new();
        let st2 = apply_with_frames(&mut frames, st, fm, roll, &hs, i18n);
        comp_applied.update_value(|v| v.push(fm)); // ответ человека — часть хода компьютера
        forced_pick.set(None);
        play_computer(frames, st2, rem, roll, None);
    };

    let click = move |target: Sel| {
        let g = game.get_untracked();
        let hs = humans.get_untracked();
        if animating.get_untracked()
            || paused.get_untracked()
            || game_over(&g.state, teams.get_untracked())
            || !hs.contains(&g.state.to_move)
            || roll.get_untracked().is_none()
        {
            return;
        }
        let ps = g.state.clone(); // доска уже продвинута применёнными частями хода
        let pre = prefix.get_untracked();
        let cands = step_opts(&turns.get_untracked(), &pre);
        // Ход «на месте» (выход из Тюрьмы «зашёл-вышел»: источник == цель) исполняем
        // одним кликом по клетке — выбора цели нет.
        if let Some(&m) = cands
            .iter()
            .find(|&&m| move_source(&ps, m) == target && move_dest(&ps, m) == target)
        {
            play_part(m);
            return;
        }
        // Ход сразу за обе кости одной фишкой: клик по конечной клетке проигрывает всю
        // последовательность и завершает ход.
        if let Some(src) = sel.get_untracked()
            && let Some((_, seq)) = combo_targets(&ps, &turns.get_untracked(), &pre, src)
                .into_iter()
                .find(|(d, _)| *d == target)
        {
            play_full(seq);
            return;
        }
        match sel.get_untracked() {
            None => {
                if cands.iter().any(|&m| move_source(&ps, m) == target) {
                    sel.set(Some(target));
                }
            }
            // Фишка всегда выбрана по умолчанию: клик по самой выбранной фишке или мимо
            // НЕ снимает выбор (раньше снимал) — переключиться можно кликом по другой.
            Some(src) if src == target => {}
            Some(src) => {
                let found = cands
                    .iter()
                    .find(|&&m| move_source(&ps, m) == src && move_dest(&ps, m) == target)
                    .copied();
                if let Some(m) = found {
                    play_part(m);
                } else if cands.iter().any(|&m| move_source(&ps, m) == target) {
                    sel.set(Some(target));
                }
            }
        }
    };

    // Горячие клавиши хода (только в партии, ход человека). Tab — следующая фишка по кругу;
    // 1/2 — ход меньшей/большей ОСТАВШЕЙСЯ костью выбранной фишкой; 3 — обе кости выбранной
    // фишкой; i — ввести фишку из резерва; r — выкупить свою пленную (если её держит один
    // соперник). Используют те же `play_part`/`play_full`, что и клики.
    window_event_listener(leptos::ev::keydown, move |e| {
        if e.ctrl_key() || e.meta_key() || e.alt_key() {
            return;
        }
        let in_field = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|el| matches!(el.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT"));
        if in_field {
            return;
        }
        // Выбор обязательного ответа на выкуп (человек-захватчик): Tab — следующий вариант,
        // Enter/Space/цифра — сыграть выбранный (или единственный). Это ход компьютера, но
        // отвечает человек, поэтому обрабатываем ДО обычных проверок «ход человека».
        if let Some((_s, captor, _r, _rem)) = forced_pick.get_untracked() {
            if animating.get_untracked() || paused.get_untracked() {
                return;
            }
            let st = game.get_untracked().state;
            let mut opts: Vec<Move> = Vec::new();
            let mut seen: Vec<usize> = Vec::new();
            for mv in forced_six_moves(&st, captor) {
                if !seen.contains(&mv.checker) {
                    seen.push(mv.checker);
                    opts.push(mv);
                }
            }
            if opts.is_empty() {
                return;
            }
            match e.code().as_str() {
                "Tab" => {
                    e.prevent_default();
                    let cur = sel.get_untracked();
                    let i = opts
                        .iter()
                        .position(|mv| Some(move_source(&st, *mv)) == cur)
                        .unwrap_or(0);
                    sel.set(Some(move_source(&st, opts[(i + 1) % opts.len()])));
                }
                "Enter" | "NumpadEnter" | "Space" | "Digit1" | "Numpad1" | "Digit2"
                | "Numpad2" | "Digit3" | "Numpad3" => {
                    e.prevent_default();
                    let cur = sel.get_untracked();
                    let chosen = opts
                        .iter()
                        .copied()
                        .find(|mv| Some(move_source(&st, *mv)) == cur)
                        .unwrap_or(opts[0]);
                    forced_apply(chosen);
                }
                _ => {}
            }
            return;
        }
        let g = game.get_untracked();
        let hs = humans.get_untracked();
        if !started.get_untracked()
            || tutorial.get_untracked()
            || dev.get_untracked()
            || animating.get_untracked()
            || paused.get_untracked()
            || rules.get_untracked()
            || forced_pick.get_untracked().is_some()
            || game_over(&g.state, teams.get_untracked())
            || !hs.contains(&g.state.to_move)
            || roll.get_untracked().is_none()
        {
            return;
        }
        let ps = g.state.clone();
        let pre = prefix.get_untracked();
        let cands = step_opts(&turns.get_untracked(), &pre);
        let code = e.code();
        match code.as_str() {
            "Tab" => {
                e.prevent_default();
                // Источники-фишки НА ДОСКЕ (резерв/плен — отдельными клавишами i/r).
                let mut sources: Vec<Sel> = Vec::new();
                for &m in &cands {
                    let s = move_source(&ps, m);
                    if matches!(s, Sel::Cell(..)) && !sources.contains(&s) {
                        sources.push(s);
                    }
                }
                if sources.is_empty() {
                    return;
                }
                let next = match sel.get_untracked() {
                    Some(s) => sources
                        .iter()
                        .position(|&x| x == s)
                        .map_or(0, |i| (i + 1) % sources.len()),
                    None => 0,
                };
                sel.set(Some(sources[next]));
            }
            "Digit1" | "Numpad1" | "Digit2" | "Numpad2" => {
                let Some(src) = sel.get_untracked() else {
                    return;
                };
                let mut rem = remaining.get_untracked();
                if rem.is_empty() {
                    return;
                }
                rem.sort_unstable();
                let smaller = matches!(code.as_str(), "Digit1" | "Numpad1");
                let die = if smaller { rem[0] } else { rem[rem.len() - 1] };
                if let Some(&m) = cands
                    .iter()
                    .find(|&&m| move_source(&ps, m) == src && m.pips == die)
                {
                    play_part(m);
                }
            }
            "Digit3" | "Numpad3" => {
                let Some(src) = sel.get_untracked() else {
                    return;
                };
                if let Some((_, seq)) =
                    combo_targets(&ps, &turns.get_untracked(), &pre, src).into_iter().next()
                {
                    play_full(seq);
                }
            }
            "KeyI" => {
                if let Some(&m) = cands.iter().find(|&&m| m.kind == MoveKind::Enter) {
                    play_part(m);
                }
            }
            "KeyR" => {
                // Выкуп — только если нашу пленную фишку держит РОВНО ОДИН соперник.
                let ransoms: Vec<Move> = cands
                    .iter()
                    .copied()
                    .filter(|&m| m.kind == MoveKind::Ransom)
                    .collect();
                let mut captors: Vec<Side> = Vec::new();
                for &m in &ransoms {
                    if let Position::Captured { captor } = ps.checkers[m.checker].pos
                        && !captors.contains(&captor)
                    {
                        captors.push(captor);
                    }
                }
                if captors.len() == 1 {
                    play_part(ransoms[0]);
                }
            }
            _ => {}
        }
    });

    // Авто-фокус: если идёт ход человека и фишка ещё не выбрана — выбираем по умолчанию
    // (первую фишку-источник НА ДОСКЕ; иначе первый источник — напр. резерв при вводе).
    // Так не нужно кликать «выбрать», особенно когда ходить можно одной фишкой; жёлтые
    // круги остальных вариантов остаются видны (см. подсветку источников).
    Effect::new(move |_| {
        if sel.get().is_some()
            || !started.get()
            || animating.get()
            || paused.get()
            || rules.get()
            || forced_pick.get().is_some()
        {
            return;
        }
        let g = game.get();
        if game_over(&g.state, teams.get_untracked())
            || !humans.get_untracked().contains(&g.state.to_move)
            || roll.get().is_none()
        {
            return;
        }
        let cands = step_opts(&turns.get(), &prefix.get());
        let mut sources: Vec<Sel> = Vec::new();
        for &m in &cands {
            let s = move_source(&g.state, m);
            if !sources.contains(&s) {
                sources.push(s);
            }
        }
        if let Some(&s) = sources
            .iter()
            .find(|s| matches!(s, Sel::Cell(..)))
            .or_else(|| sources.first())
        {
            sel.set(Some(s));
        }
    });

    // Запуск партии по выбранным настройкам (число игроков и кто человек).
    let start_game = move |_| {
        // Новое поколение — глушит анимации предыдущей партии (иначе две идут разом).
        epoch.update_value(|e| *e += 1);
        let active = active_for(players.get_untracked());
        let game_seed = seed();
        let teams_on = teams.get_untracked() && active.len() == 4;
        dice.update_value(|d| *d = RandomDice::from_seed(game_seed));
        roll.set(None);
        turns.set(Vec::new());
        prefix.set(Vec::new());
        sel.set(None);
        finished.set(Vec::new());
        let hs = humans.get_untracked();
        // Технический лог партии (для воспроизведения багов) — только в отладке.
        dbg_log_reset();
        dbg_log(&format!(
            "[GAMELOG] === new game === seed={game_seed} players={} humans={hs:?} teams={teams_on}",
            active.len()
        ));
        let g = fresh(dice, active, teams_on);
        herald.set(tu_string!(i18n, herald_game_on, who = side_name(g.state.to_move, &hs, i18n)).to_string());
        game.set(g);
        started.set(true);
        kickoff();
    };
    // Назад к настройкам / «Закончить игру»: глушим анимации текущей партии и
    // сбрасываем переходные сигналы, чтобы новая партия стартовала с чистого листа.
    let to_settings = move |_| {
        epoch.update_value(|e| *e += 1);
        animating.set(false);
        rolling.set(false);
        roll.set(None);
        started.set(false);
    };

    // Демо-анимация (режим разработчика): пошаговый ход со съеданием через тот же
    // конвейер кадров, что и в партии (бросок, шаги по клеткам, реплики ведущего).
    let anim_demo = move |_| {
        if animating.get_untracked() {
            return;
        }
        epoch.update_value(|e| *e += 1);
        let mut gg = demo_capture();
        let r = DiceRoll::new(Die::new(3).expect("3"), Die::new(1).expect("1"));
        let t = legal_turns(&gg.state, r);
        if t.is_empty() {
            return;
        }
        let idx = Heuristic.choose_turn(&gg.state, &t).min(t.len() - 1);
        let played = t[idx].clone();
        game.set(Game::new(gg.state.clone()));
        roll.set(None);
        let mut frames = Vec::new();
        commit_frames(&mut frames, &mut gg, r, played, &[], true, |s, c, o| {
            Heuristic.choose_forced(s, c, o)
        }, i18n);
        frames.push(Frame {
            state: gg.state.clone(),
            roll: None,
            hold: HOLD_STEP_MS,
            rolling: false,
            note: Some(tu_string!(i18n, demo_complete).to_string()),
            pts: Vec::new(),
            fade: false,
            sound: None,
        });
        play(frames);
    };

    // Тест входа в Дом: стартует ИНТЕРАКТИВНУЮ партию из расстановки, где фишки A стоят
    // вплотную к Дому, с заранее заданным броском (1,2) — чтобы сразу проверить клик по
    // клетке входа (воротам). Бросок задаём вручную, минуя авто-бросок (роль уже `Some`).
    let home_test = move |_| {
        epoch.update_value(|e| *e += 1);
        animating.set(false);
        let r = DiceRoll::new(Die::new(1).expect("1"), Die::new(2).expect("2"));
        let st = home_entry_test();
        let t = legal_turns(&st, r);
        humans.set(vec![Side::A]);
        finished.set(Vec::new());
        dbg_log_reset();
        dbg_log("[GAMELOG] === dev: home-entry test ===");
        game.set(Game::new(st));
        roll.set(Some(r));
        turns.set(t);
        prefix.set(Vec::new());
        remaining.set(r.values().to_vec());
        sel.set(None);
        herald.set(tu_string!(i18n, home_test_herald).to_string());
        dev.set(false);
        started.set(true);
        kickoff();
    };

    // Открыть/закрыть экран разработчика, показав состояние-заглушку.
    let open_dev = move |_| {
        epoch.update_value(|e| *e += 1);
        animating.set(false);
        game.set(Game::new(demo_game(Demo::Reserve)));
        roll.set(None);
        herald.set(tu_string!(i18n, dev_title).to_string());
        dev.set(true);
    };

    // Переключатель типа стороны (человек ↔ компьютер) в настройках.
    let toggle_human = move |side: Side| {
        humans.update(|h| {
            if let Some(i) = h.iter().position(|&s| s == side) {
                h.remove(i);
            } else {
                h.push(side);
            }
        });
    };

    view! {
        <Title text=move || t_string!(i18n, app_title) />
        <div class="wrap">
            // Заголовок — только на экране настроек; в партии/обучении он лишь крал бы
            // высоту у доски.
            {move || (!started.get() && !dev.get() && !rules.get() && !about.get() && !tutorial.get())
                .then(|| view! { <h1>{t_string!(i18n, app_title)}</h1> })}
            // Экран настроек до старта партии: число игроков и тип каждой стороны.
                    {move || (!started.get() && !dev.get() && !rules.get() && !about.get() && !tutorial.get()).then(|| view! {
                <div class="settings">
                    <div class="set-row">
                        <span>{t!(i18n, settings_players)}</span>
                        {[2usize, 3, 4].into_iter().map(|n| view! {
                            <button
                                class:on=move || players.get() == n
                                on:click=move |_| { players.set(n); humans.set(vec![Side::A]); }
                            >{n.to_string()}</button>
                        }).collect_view()}
                    </div>
                    {move || {
                        let act = active_for(players.get());
                        act.into_iter().map(|s| {
                            let is_h = move || humans.get().contains(&s);
                            view! {
                                <div class="set-row">
                                    <b style=format!("color:{}", side_color(s))>
                                        {t_string!(i18n, settings_side, letter = s.letter().to_string())}
                                    </b>
                                    <button class:on=is_h on:click=move |_| toggle_human(s)>
                                        {move || if is_h() { t_string!(i18n, settings_human).to_string() } else { t_string!(i18n, settings_computer).to_string() }}
                                    </button>
                                    // Для компьютера — выбор алгоритма (MLP/Linear/эвристика).
                                    {move || (!is_h()).then(|| view! {
                                        <span class="algo-pick">
                                            {Algo::ALL.into_iter().map(|a| {
                                                let on = move || algos.get()[s.index()] == a;
                                                view! {
                                                    <button class:on=on
                                                        on:click=move |_| algos.update(|arr| arr[s.index()] = a)>
                                                        {a.label(i18n)}
                                                    </button>
                                                }
                                            }).collect_view()}
                                        </span>
                                    })}
                                </div>
                            }
                        }).collect_view()
                    }}
                    // Режим вчетвером: команды 2×2 (A+C vs B+D) или каждый сам за себя.
                    {move || (players.get() == 4).then(|| view! {
                        <div class="set-row">
                            <span>{t!(i18n, settings_mode)}</span>
                            <button class:on=move || teams.get() on:click=move |_| teams.set(true)>
                                {t!(i18n, settings_teams)}
                            </button>
                            <button class:on=move || !teams.get() on:click=move |_| teams.set(false)>
                                {t!(i18n, settings_ffa)}
                            </button>
                        </div>
                    })}
                    // Скорость анимации — видна сразу (ползунок, без кнопки).
                    <div class="set-row"><SpeedControl speed i18n=i18n/></div>
                    <button class="primary" on:click=start_game>{t!(i18n, settings_start)}</button>
                    <div class="set-row">
                        <button on:click=move |_| {
                            epoch.update_value(|e| *e += 1);
                            animating.set(false);
                            rolling.set(false);
                            lesson_idx.set(0);
                            tut_sel.set(false);
                            tut_played.set(0);
                            tut_pick.set(None);
                            lessons_sv.with_value(|ls| {
                                game.set(Game::new(ls[0].before.clone()));
                                // Первый бросок — до первого хода.
                                match ls[0].roll {
                                    Some(r) => play(roll_only_frames(&ls[0].before, r)),
                                    None => roll.set(None),
                                }
                            });
                            tutorial.set(true);
                        }>{t!(i18n, settings_tutorial)}</button>
                        <button on:click=move |_| rules.set(true)>{t!(i18n, settings_rules)}</button>
                        <button on:click=move |_| about.set(true)>{t!(i18n, settings_about)}</button>
                        <a class="icon-btn" href="https://github.com/kakserpom/sheshbesh" target="_blank" title="GitHub">{view! {
                            <svg viewBox="0 0 16 16" fill="currentColor" width="1.2em" height="1.2em">
                                <path d="M8 0C3.58 0 0 3.58 0 8a8 8 0 0 0 5.47 7.59c.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82a7.42 7.42 0 0 1 4 0c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8z"/>
                            </svg>
                        }}</a>
                        // Переключение языка — выпадающее меню как у темы.
                        <LocaleControl i18n menu_open=locale_menu/>
                        // Выбор темы оформления — иконка-палитра с выпадающим списком.
                        <ThemeControl theme menu_open=theme_menu i18n=i18n/>
                        // Режим разработчика — только в отладочной сборке.
                        {cfg!(debug_assertions).then(|| view! {
                            <button class="icon-btn" title=t_string!(i18n, settings_dev) on:click=open_dev>"🛠"</button>
                        })}
                    </div>
                </div>
            })}

            // Экран правил: структурированный справочник (самобытные правила).
            {move || rules.get().then(|| view! {
                <div class="rules">
                    <div class="page-head">
                        <button class="icon-btn" title=t_string!(i18n, tutorial_back) on:click=move |_| rules.set(false)>"←"</button>
                        <h2 inner_html=t_string!(i18n, rules_title) />
                    </div>
                    <p class="lead" inner_html=t_string!(i18n, rules_lead) />

                    <h3 inner_html=t_string!(i18n, rules_goal_title) />
                    <p inner_html=t_string!(i18n, rules_goal_text) />

                    <h3 inner_html=t_string!(i18n, rules_first_title) />
                    <p inner_html=t_string!(i18n, rules_first_text) />

                    <h3 inner_html=t_string!(i18n, rules_board_title) />
                    <ul>
                        <li inner_html=t_string!(i18n, rules_board_l1) />
                        <li inner_html=t_string!(i18n, rules_board_l2) />
                        <li inner_html=t_string!(i18n, rules_board_l3) />
                    </ul>

                    <h3 inner_html=t_string!(i18n, rules_move_title) />
                    <ul>
                        <li inner_html=t_string!(i18n, rules_move_l1) />
                        <li inner_html=t_string!(i18n, rules_move_l2) />
                        <li inner_html=t_string!(i18n, rules_move_l3) />
                        <li inner_html=t_string!(i18n, rules_move_l4) />
                        <li inner_html=t_string!(i18n, rules_move_l5) />
                        <li inner_html=t_string!(i18n, rules_move_l6) />
                        <li inner_html=t_string!(i18n, rules_move_l7) />
                    </ul>

                    <h3 inner_html=t_string!(i18n, rules_special_title) />
                    <ul>
                        <li inner_html=t_string!(i18n, rules_special_l1) />
                        <li inner_html=t_string!(i18n, rules_special_l2) />
                        <li inner_html=t_string!(i18n, rules_special_l3) />
                    </ul>

                    <h3 inner_html=t_string!(i18n, rules_capture_title) />
                    <ul>
                        <li inner_html=t_string!(i18n, rules_capture_l1) />
                        <li inner_html=t_string!(i18n, rules_capture_l2) />
                        <li inner_html=t_string!(i18n, rules_capture_l3) />
                    </ul>

                    <h3 inner_html=t_string!(i18n, rules_home_title) />
                    <p inner_html=t_string!(i18n, rules_home_text) />
                </div>
            })}

            // Экран «Об игре»: личная история происхождения этой самобытной игры.
            {move || about.get().then(|| view! {
                <div class="rules">
                    <div class="page-head">
                        <button class="icon-btn" title=t_string!(i18n, tutorial_back) on:click=move |_| about.set(false)>"←"</button>
                        <h2 inner_html=t_string!(i18n, about_title) />
                    </div>
                    <p inner_html=t_string!(i18n, about_p1) />
                    <p inner_html=t_string!(i18n, about_p2) />
                    <p inner_html=t_string!(i18n, about_p3) />
                    <p inner_html=t_string!(i18n, about_p4) />
                </div>
            })}

            // Экран обучения: пошаговые уроки с анимацией. Доска и кубик — те же, что
            // в игре (сигнал `game` + `play` + keyed-фишки со скольжением).
            {move || (tutorial.get() && !rules.get()).then(|| {
                let total = lessons_sv.with_value(Vec::len);
                // Завершает кадры хода игрока: проигрывает авто-ход соперника (если есть)
                // — чтобы стороны ходили по очереди — затем бросает кости СЛЕДУЮЩЕГО
                // шага (бросок до хода) и переходит к нему.
                let finish_step = move |frames: &mut Vec<Frame>, after: &GameState, cur: usize| {
                    lessons_sv.with_value(|ls| {
                        // Ход соперника: его бросок + пошаговое движение его фишки.
                        // В этих кадрах `to_move = C`, чтобы кости рисовались у Дома C.
                        let end_state = match &ls[cur].opp {
                            Some(opp) => {
                                let mut st = after.clone();
                                st.to_move = Side::C;
                                frames.extend(roll_only_frames(&st, opp.roll));
                                for mv in &opp.moves {
                                    st = apply_with_frames(frames, st, *mv, opp.roll, &[], i18n);
                                }
                                st.to_move = Side::A; // вернули очередь игроку
                                st
                            }
                            None => after.clone(),
                        };
                        let _ = end_state;
                        if cur + 1 < ls.len() {
                            // Бросок следующего шага — на ЕГО позиции `before` (она же = конец
                            // хода для непрерывных шагов; для отдельных — переход к новой).
                            let nb = ls[cur + 1].before.clone();
                            let next_commit = ls[cur + 1].commit;
                            // Шаг-«телепорт» (расстановка задана вручную): помечаем первый
                            // кадр перехода на fade — доска погаснет, сменится и проявится,
                            // чтобы фишки не «перелетали» через всю доску.
                            let teleport = ls[cur + 1].teleport;
                            let mark = frames.len();
                            match ls[cur + 1].roll {
                                Some(nr) => {
                                    frames.extend(roll_only_frames(&nb, nr));
                                    // Шаг-выкуп: сразу проигрываем ход соперника (возврат
                                    // пленной фишки в резерв), оставляя игроку лишь выбор
                                    // обязательного ответного хода на 6 (фаза 1).
                                    if next_commit {
                                        let mut s = nb;
                                        for mv in &ls[cur + 1].moves {
                                            s = apply_with_frames(frames, s, *mv, nr, &[], i18n);
                                        }
                                    }
                                }
                                None => frames.push(Frame { state: nb, roll: None, hold: HOLD_STEP_MS, rolling: false, note: None, pts: Vec::new(), fade: false, sound: None }),
                            }
                            if teleport && let Some(f) = frames.get_mut(mark) {
                                f.fade = true;
                            }
                            lesson_idx.set(cur + 1);
                            // Для шага-выкупа сразу переходим к фазе выбора хода на 6.
                            tut_played.set(usize::from(next_commit));
                        } else {
                            // Последний шаг отыгран: помечаем его полностью сыгранным
                            // (`tut_played == moves.len()`), чтобы интерактивный слой
                            // СКРЫЛСЯ. Иначе он остался бы на последнем шаге (lesson_idx
                            // не растёт) и попытался бы переиграть ход с уже сыгранной
                            // позиции — `apply` на ставшем нелегальным ходе паникует, и
                            // слой кликов «умирает»: после этого ход не сделать вручную
                            // даже вернувшись на прошлые шаги.
                            tut_played.set(ls[cur].moves.len());
                        }
                    });
                    tut_pick.set(None);
                };
                // Проиграть `count` под-ходов текущего шага начиная с уже сыгранных
                // (1 — по клику на клетку; usize::MAX — весь оставшийся ход по ▶).
                let play_submoves = move |count: usize| {
                    if animating.get_untracked() {
                        return;
                    }
                    let cur = lesson_idx.get_untracked();
                    lessons_sv.with_value(|ls| {
                        // Шаг-выкуп в две фазы: сперва соперник выкупает пленную, затем
                        // игрок ВЫБИРАЕТ обязательный ход захватчика на 6.
                        if ls[cur].commit {
                            let Some(r) = ls[cur].roll else { return };
                            let st = game.get_untracked().state.clone();
                            if tut_played.get_untracked() == 0 {
                                // Фаза 0: ход соперника — выкуп пленной фишки.
                                let mut frames = Vec::new();
                                let mut s = st;
                                for mv in &ls[cur].moves {
                                    s = apply_with_frames(&mut frames, s, *mv, r, &[], i18n);
                                }
                                tut_sel.set(false);
                                tut_played.set(1); // дальше — выбор хода на 6
                                play(frames);
                            } else {
                                // Фаза 1: ▶ выбирает первый из обязательных ходов на 6.
                                let opts = forced_six_moves(&st, Side::A);
                                let mut frames = Vec::new();
                                let end = match opts.first() {
                                    Some(mv) => apply_with_frames(&mut frames, st, *mv, r, &[], i18n),
                                    None => st,
                                };
                                tut_sel.set(false);
                                tut_pick.set(None);
                                finish_step(&mut frames, &end, cur);
                                play(frames);
                            }
                            return;
                        }
                        match ls[cur].roll {
                            Some(r) => {
                                let chosen = &ls[cur].moves;
                                let k = tut_played.get_untracked();
                                let end = k.saturating_add(count).min(chosen.len());
                                let mut state = game.get_untracked().state.clone();
                                let mut frames = Vec::new();
                                for mv in &chosen[k..end] {
                                    state = apply_with_frames(&mut frames, state, *mv, r, &[], i18n);
                                }
                                tut_sel.set(false);
                                if end >= chosen.len() {
                                    finish_step(&mut frames, &state, cur);
                                } else {
                                    tut_played.set(end);
                                }
                                play(frames);
                            }
                            None => {
                                // Шаг-пояснение без хода — просто показываем следующий.
                                if cur + 1 < ls.len() {
                                    let nb = ls[cur + 1].before.clone();
                                    lesson_idx.set(cur + 1);
                                    tut_played.set(0);
                                    game.set(Game::new(nb.clone()));
                                    match ls[cur + 1].roll {
                                        Some(nr) => play(roll_only_frames(&nb, nr)),
                                        None => roll.set(None),
                                    }
                                }
                            }
                        }
                    });
                };
                // Прыжок на произвольный шаг (выпадающий список и кнопка ◀): сперва
                // ГАСИМ доску (fade-out), затем — пока она невидима — меняем расстановку
                // «вслепую», и только потом ПРОЯВЛЯЕМ (fade-in). Так нет «перелёта» фишек
                // и доска не подменяется на глазах. Смену делает только АКТУАЛЬНА
                // навигация (проверка `era`); если её сменила новая — та поставит своё.
                let goto_lesson = move |i: usize| {
                    epoch.update_value(|e| *e += 1);
                    let era = epoch.get_value();
                    animating.set(false);
                    rolling.set(false);
                    tut_sel.set(false);
                    tut_pick.set(None);
                    fading.set(true); // 1) fade-out
                    spawn_local(async move {
                        TimeoutFuture::new(FADE_MS).await; // ждём, пока доска погаснет
                        if epoch.get_value() != era {
                            return; // навигацию сменила новая — она поставит своё состояние
                        }
                        // 2) доска невидима — меняем расстановку «вслепую».
                        lesson_idx.set(i);
                        lessons_sv.with_value(|ls| {
                            let l = &ls[i];
                            if l.commit {
                                // Шаг-выкуп: соперник УЖЕ выкупил фишку (фаза 0 неинтерактивна),
                                // сразу даём игроку фазу 1 — обязательный ход захватчика на 6.
                                let mut st = l.before.clone();
                                for mv in &l.moves {
                                    st = apply(&st, *mv);
                                }
                                game.set(Game::new(st));
                                tut_played.set(1);
                            } else {
                                game.set(Game::new(l.before.clone()));
                                tut_played.set(0);
                            }
                            roll.set(l.roll);
                        });
                        // 3) даём кадр на отрисовку новой расстановки и проявляем доску.
                        TimeoutFuture::new(30).await;
                        if epoch.get_value() == era {
                            fading.set(false); // fade-in
                        }
                    });
                };
                // Позиции всплывающих подсказок для ключевых зон на доске (сторона A).
                let btm = (BOARD_MARGIN + SIDE_LEN - 1) as f64 + 0.5;
                let entry_pt = center_pt(margin_coord(Side::A.entry()));
                let emid = entry_pt.0;
                let moon_pt = moon_arc_point(Side::A, 0.5);
                let prison_cell_pt = center_pt(margin_coord(
                    Side::A.local_to_perimeter(LOCAL_PRISON_NEAR),
                ));
                view! {
                    <div class="status">
                        <div class="status-left">
                        <button class="icon-btn" title=t_string!(i18n, tutorial_back) on:click=move |_| { epoch.update_value(|e| *e += 1); animating.set(false); rolling.set(false); tutorial.set(false); }>"←"</button>
                        <button class="icon-btn hamburger-btn" class:on=move || hamburger.get()
                            title=t_string!(i18n, hamburger_menu) on:click=move |_| hamburger.update(|h| *h = !*h)>"☰"</button>
                        <div class="hamburger-content" class:open=move || hamburger.get()>
                        <button class="hamburger-item" on:click=move |_| { rules.set(true); hamburger.set(false); }>"📖" <span class="hamburger-label">{t_string!(i18n, hamburger_rules)}</span></button>
                        <button class="hamburger-item" on:click=move |_| sound.update(|s| *s = !*s)>
                            {move || if sound.get() { "🔊" } else { "🔇" }}<span class="hamburger-label">{t_string!(i18n, hamburger_sound)}</span>
                        </button>
                <div class="hamburger-item-wrap">
                <button class="hamburger-item hamburger-locale-btn" on:click=move |_| locale_menu.update(|o| *o = !*o)>
                    "🌐" <span class="hamburger-label">{t_string!(i18n, settings_lang)}</span>
                </button>
                        {move || locale_menu.get().then(|| view! {
                            <div class="hamburger-submenu">
                                <button class:on=move || i18n.get_locale() == Locale::ru
                                    on:click=move |_| {
                                        let _ = web_sys::window().and_then(|w| w.local_storage().ok().flatten().map(|s| s.set_item("locale", "ru").ok()));
                                        i18n.set_locale(Locale::ru);
                                        locale_menu.set(false);
                                    }>"Русский"</button>
                                <button class:on=move || i18n.get_locale() == Locale::en
                                    on:click=move |_| {
                                        let _ = web_sys::window().and_then(|w| w.local_storage().ok().flatten().map(|s| s.set_item("locale", "en").ok()));
                                        i18n.set_locale(Locale::en);
                                        locale_menu.set(false);
                                    }>"English"</button>
                            </div>
                        })}
                        </div>
                        <div class="hamburger-item-wrap">
                        <button class="hamburger-item hamburger-speed-menu" on:click=move |_| speed_menu.update(|o| *o = !*o)>
                            "⏩" <span class="hamburger-label">{t_string!(i18n, hamburger_speed)}</span>
                        </button>
                        {move || speed_menu.get().then(|| view! {
                            <div class="hamburger-submenu hamburger-speed-menu">
<SpeedControl speed i18n=i18n/>
                            </div>
                        })}
                        </div>
                        <div class="hamburger-item-wrap">
                        <button class="hamburger-item hamburger-theme-menu" on:click=move |_| theme_menu.update(|o| *o = !*o)>
                            "🎨" <span class="hamburger-label">{t_string!(i18n, hamburger_theme)}</span>
                        </button>
                        {move || theme_menu.get().then(|| view! {
                            <div class="hamburger-submenu hamburger-theme-menu">
                                {Theme::ALL.into_iter().map(|t| {
                                    let on = move || theme.get() == t;
                                    view! {
                                        <button class:on=on on:click=move |_| theme.set(t)>
                                            {move || match t {
                                                Theme::Midnight => t_string!(i18n, theme_midnight),
                                                Theme::Dusk => t_string!(i18n, theme_dusk),
                                                Theme::Daylight => t_string!(i18n, theme_daylight),
                                                Theme::Forest => t_string!(i18n, theme_forest),
                                            }}
                                        </button>
                                    }
                                }).collect_view()}
                            </div>
                        })}
                        </div>
                        </div>
                        </div>
                        // Выпадающий список шагов — прыжок на любой шаг обучения.
                        <div class="lesson-pick">
                            <span class="lesson-label">{t_string!(i18n, tutorial_lesson)}</span>
                            <select class="lesson-select"
                                prop:value=move || lesson_idx.get().to_string()
                                on:change=move |ev| {
                                    let i: usize = event_target_value(&ev).parse().unwrap_or(0);
                                    goto_lesson(i);
                                }>
                                {lessons_sv.with_value(|ls| ls.iter().enumerate().map(|(i, l)| view! {
                                    <option value=i.to_string()>
                                        {format!("{}/{total}: {}", i + 1, l.title)}
                                    </option>
                                }).collect_view())}
                            </select>
                        </div>
                    </div>
                    <div class="board-area">
                    <div class="board-wrap" class:faded=move || fading.get()>
                    <svg class="board" viewBox=format!(
                        "{o} {o} {s} {s}",
                        o = BOARD_MARGIN as f64 - RESERVE_PAD,
                        s = (BOARD_DIM - 2 * BOARD_MARGIN) as f64 + 2.0 * RESERVE_PAD,
                    )>
                        {move || static_board(&game.get().state)}
                        {view! {
                            <g class="tut-labels">
                                <g class="tut-label">
                                    <text x=emid - 1.5 y=btm + 0.9 class="tut-text" text-anchor="middle" dy="0.35em">
                                        <title>{t_string!(i18n, tutorial_tip_home)}</title>
                                        {t_string!(i18n, tutorial_label_home)}
                                    </text>
                                </g>
                                <g class="tut-label">
                                    <text x=moon_pt.0 y=moon_pt.1 + 1.0 class="tut-text" text-anchor="middle" dy="0.35em">
                                        <title>{t_string!(i18n, tutorial_tip_moon)}</title>
                                        {t_string!(i18n, tutorial_label_moon)}
                                    </text>
                                </g>
                                <g class="tut-label">
                                    <text x=prison_cell_pt.0 y=btm + 0.9 class="tut-text" text-anchor="middle" dy="0.35em">
                                        <title>{t_string!(i18n, tutorial_tip_prison)}</title>
                                        {t_string!(i18n, tutorial_label_prison)}
                                    </text>
                                </g>
                                <g class="tut-label">
                                    <text x=emid y=btm + 0.9 class="tut-text" text-anchor="middle" dy="0.35em">
                                        <title>{t_string!(i18n, tutorial_tip_entry)}</title>
                                        {t_string!(i18n, tutorial_label_entry)}
                                    </text>
                                </g>
                            </g>
                        }}
                        <For each=move || 0..game.get().state.checkers.len() key=|i| *i let:i>
                            <circle
                                r=move || match game.get().state.checkers[i].pos { Position::Prison { .. } | Position::Captured { .. } => 0.22, Position::Reserve => 0.3, _ => 0.36 }
                                class=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Captured { .. }) { "piece captive" } else { "piece" }
                                fill=move || side_color(game.get().state.checkers[i].owner)
                                cx=move || anim_xy(&game.get().state, i, &anim_pts.get()).0
                                cy=move || anim_xy(&game.get().state, i, &anim_pts.get()).1
                                opacity=move || if anim_xy(&game.get().state, i, &anim_pts.get()).2 { 1.0 } else { 0.0 }
                            />
                        </For>
                        <For each=move || stack_counts(&game.get().state) key=|b| b.key let:b>
                            {badge_view(game, b.key)}
                        </For>
                        // Интерактивный ход кликами ПО ОДНОМУ под-ходу (по каждой кости):
                        // фишка → клетка её следующего под-хода. Не во время движения.
                        {move || (!animating.get()).then(|| {
                            let cur = lesson_idx.get().min(total - 1);
                            lessons_sv.with_value(|ls| {
                                let mut nodes: Vec<AnyView> = Vec::new();
                                if ls[cur].roll.is_some() && !ls[cur].commit {
                                    let chosen = &ls[cur].moves;
                                    let k = tut_played.get();
                                    let st = game.get().state;
                                    // Слой рисуем, только если очередной под-ход ЛЕГАЛЕН из
                                    // текущей позиции — иначе `apply` ниже паниковал бы (так
                                    // было на завершённом последнем шаге).
                                    if k < chosen.len() && move_legal(&st, chosen[k]) {
                                        let mv = chosen[k];
                                        let after = apply(&st, mv);
                                        let moved = mv.checker;
                                        let (sx, sy, _) = checker_xy(&st, moved);
                                        let sel = tut_sel.get();
                                        let src_cls = if sel { "hl-sel" } else { "hl-src" };
                                        nodes.push(view! { <circle cx=sx cy=sy r=0.47 fill="none" class=src_cls /> }.into_any());
                                        nodes.push(view! { <circle cx=sx cy=sy r=0.5 class="hit" on:click=move |_| tut_sel.set(true) /> }.into_any());
                                        if sel {
                                            // Цель: для входа на Луну — клетка-вход (на
                                            // периметре), для остального — куда реально
                                            // встанет фишка (поле дорожки/клетка/каземат).
                                            let (tx, ty) = match (mv.kind, after.checkers[moved].pos) {
                                                (MoveKind::EnterMoon, Position::Moon { side, .. }) => {
                                                    center_pt(margin_coord(side.local_to_perimeter(LOCAL_MOON)))
                                                }
                                                // Вход в Тюрьму — на саму клетку, а не в каземат.
                                                (MoveKind::EnterPrison, Position::Prison { cell }) => {
                                                    center_pt(margin_coord(cell))
                                                }
                                                _ => {
                                                    let (x, y, _) = checker_xy(&after, moved);
                                                    (x, y)
                                                }
                                            };
                                            nodes.push(view! { <circle cx=tx cy=ty r=0.47 fill="none" class="hl-dst" /> }.into_any());
                                            nodes.push(view! { <circle cx=tx cy=ty r=0.5 class="hit" on:click=move |_| play_submoves(1) /> }.into_any());
                                        }
                                    }
                                } else if ls[cur].commit && tut_played.get() == 1 {
                                    // Выбор обязательного хода захватчика на 6: игрок сам
                                    // решает, какой фишкой ходить (фишка → её клетка +6).
                                    let r = ls[cur].roll.unwrap_or(dr(6, 6));
                                    let st = game.get().state;
                                    let pick = tut_pick.get();
                                    let mut shown: Vec<usize> = Vec::new();
                                    for mv in forced_six_moves(&st, Side::A) {
                                        if shown.contains(&mv.checker) {
                                            continue;
                                        }
                                        shown.push(mv.checker);
                                        let ci = mv.checker;
                                        let (sx, sy, _) = checker_xy(&st, ci);
                                        let selected = pick == Some(ci);
                                        let src_cls = if selected { "hl-sel" } else { "hl-src" };
                                        nodes.push(view! { <circle cx=sx cy=sy r=0.47 fill="none" class=src_cls /> }.into_any());
                                        nodes.push(view! { <circle cx=sx cy=sy r=0.5 class="hit" on:click=move |_| tut_pick.set(Some(ci)) /> }.into_any());
                                        if selected {
                                            let (tx, ty, _) = checker_xy(&apply(&st, mv), ci);
                                            nodes.push(view! { <circle cx=tx cy=ty r=0.47 fill="none" class="hl-dst" /> }.into_any());
                                            nodes.push(view! { <circle cx=tx cy=ty r=0.5 class="hit"
                                                on:click=move |_| {
                                                    if animating.get_untracked() { return; }
                                                    let st = game.get_untracked().state.clone();
                                                    let mut frames = Vec::new();
                                                    let end = apply_with_frames(&mut frames, st, mv, r, &[], i18n);
                                                    tut_pick.set(None);
                                                    finish_step(&mut frames, &end, cur);
                                                    play(frames);
                                                } /> }.into_any());
                                        }
                                    }
                                }
                                nodes
                            })
                        })}
                    </svg>
                    // Две кости у Дома ХОДЯЩЕЙ стороны (в кадрах хода соперника `to_move`
                    // = C) — как в реальной игре, через тот же сигнал `roll`.
                    {move || roll.get().map(|r| {
                        let [a, b] = r.values();
                        let side = game.get().state.to_move;
                        let (ax, ay) = outside_anchor(side, DICE_OUT);
                        let (out, _) = reserve_axes(side);
                        let vertical = out.0 != 0.0;
                        let vb_o = BOARD_MARGIN as f64 - RESERVE_PAD;
                        let vb_s = (BOARD_DIM - 2 * BOARD_MARGIN) as f64 + 2.0 * RESERVE_PAD;
                        let fx = (ax - vb_o) / vb_s * 100.0;
                        let fy = (ay - vb_o) / vb_s * 100.0;
                        let inner = if rolling.get() {
                            view! { <span class="dice rolling">{die3d(a)} {die3d(b)}</span> }.into_any()
                        } else {
                            view! { <span class="dice">{die_face(a)} {die_face(b)}</span> }.into_any()
                        };
                        view! { <div class="board-dice" class:vert=vertical style=format!("left:{fx:.2}%;top:{fy:.2}%")>{inner}</div> }
                    })}
                    // Навигация по шагам — стрелки по краям доски.
                    <button class="nav-arrow left" title=t_string!(i18n, tutorial_prev)
                        on:click=move |_| {
                            // Назад — через тот же fade-переход, что и выпадающий список.
                            let i = lesson_idx.get_untracked().saturating_sub(1);
                            goto_lesson(i);
                        }>"◀"</button>
                    <button class="nav-arrow right" title=t_string!(i18n, tutorial_next)
                        on:click=move |_| play_submoves(usize::MAX)>"▶"</button>
                    </div>
                    </div>
                    // Подвал урока фиксированной высоты — текст/подсказка меняются, но
                    // не отнимают высоту у доски (иначе её масштаб скакал бы).
                    <div class="lesson-foot">
                    <p class="lesson-text">{move || {
                        // Пока кости кувыркаются — нейтральная заглушка: текст урока
                        // называет выпавшие значения и спойлерил бы ещё не вставший бросок.
                        // Кости противника (ход C) подписываем отдельно.
                        if rolling.get() {
                            if game.get().state.to_move == Side::A {
                                t_string!(i18n, tutorial_hint_roll).to_string()
                            } else {
                                t_string!(i18n, tutorial_hint_opp_roll).to_string()
                            }
                        } else {
                            lessons_sv.with_value(|ls| ls[lesson_idx.get().min(total - 1)].text.to_string())
                        }
                    }}</p>
                    {move || lessons_sv.with_value(|ls| {
                        let cur = lesson_idx.get().min(total - 1);
                        if rolling.get() {
                            // Во время броска ход ещё нельзя делать — подсказку прячем.
                            ().into_any()
                        } else if ls[cur].commit {
                            (tut_played.get() == 1).then(|| view! {
                                <p class="lesson-hint">{t_string!(i18n, tutorial_hint_ransom)}</p>
                            }).into_any()
                        } else if ls[cur].roll.is_some() {
                            view! {
                                <p class="lesson-hint">{t_string!(i18n, tutorial_hint_move)}</p>
                            }.into_any()
                        } else {
                            ().into_any()
                        }
                    })}
                    </div>
                    // Плашка завершения обучения (последний шаг, не анимируется).
                    {move || {
                        let cur = lesson_idx.get();
                        if cur + 1 >= total && !animating.get() && !rolling.get() {
                            view! {
                                <div class="complete-overlay">
                                    <h2>{t_string!(i18n, tutorial_complete)}</h2>
                                    <div class="complete-buttons">
                                        <button class="primary" on:click=move |_| goto_lesson(0)>
                                            {t_string!(i18n, tutorial_complete_retry)}
                                        </button>
                                        <button on:click=move |_| { epoch.update_value(|e| *e += 1); animating.set(false); rolling.set(false); tutorial.set(false); }>
                                            {t_string!(i18n, tutorial_complete_back)}
                                        </button>
                                    </div>
                                </div>
                            }.into_any()
                        } else {
                            ().into_any()
                        }
                    }}
                }
            })}

            // Экран разработчика: панель сценариев + демо-анимация + read-only доска.
            {move || dev.get().then(|| view! {
                <div class="status">
                    <div class="status-left">
                    <button class="icon-btn" title=t_string!(i18n, dev_back) on:click=move |_| { epoch.update_value(|e| *e += 1); animating.set(false); dev.set(false); }>"←"</button>
                    </div>
                    <span class="herald" inner_html=move || herald.get()></span>
                </div>
                <div class="controls dev-controls">
                    {DEMOS.iter().map(|d| {
                        let demo_label = move || match d {
                            Demo::Reserve => t_string!(i18n, demo_reserve),
                            Demo::Moon(MoonField::One) => t_string!(i18n, demo_moon_1),
                            Demo::Moon(MoonField::Three) => t_string!(i18n, demo_moon_3),
                            Demo::Moon(MoonField::Six) => t_string!(i18n, demo_moon_6),
                            Demo::Cell => t_string!(i18n, demo_cell),
                            Demo::Prison => t_string!(i18n, demo_prison),
                            Demo::PrisonStack => t_string!(i18n, demo_prison_stack),
                            Demo::Captured => t_string!(i18n, demo_captured),
                            Demo::Homes => t_string!(i18n, demo_homes),
                        };
                        let demo = *d;
                        view! {
                            <button on:click=move |_| {
                                epoch.update_value(|e| *e += 1);
                                animating.set(false);
                                game.set(Game::new(demo_game(demo)));
                                roll.set(None);
                                herald.set(demo_label().to_string());
                            }>{demo_label().to_string()}</button>
                        }
                    }).collect_view()}
                    <button on:click=anim_demo>{t!(i18n, demo_anim)}</button>
                    <button on:click=home_test>{t!(i18n, demo_home_test)}</button>
                </div>
                // Панель прослушивания звуков: по кнопке на каждое событие.
                <div class="controls dev-controls">
                    <span class="set-name">{t_string!(i18n, dev_sounds)}</span>
                    {settings::SoundKind::ALL.iter().map(|&(kind, label)| view! {
                        <button on:click=move |_| settings::play(kind)>{label}</button>
                    }).collect_view()}
                </div>
                <div class="board-area">
                <div class="board-wrap">
                <svg class="board" viewBox=format!(
                    "{o} {o} {s} {s}",
                    o = BOARD_MARGIN as f64 - RESERVE_PAD,
                    s = (BOARD_DIM - 2 * BOARD_MARGIN) as f64 + 2.0 * RESERVE_PAD,
                )>
                    {move || static_board(&game.get().state)}
                    <For each=move || 0..game.get().state.checkers.len() key=|i| *i let:i>
                        <circle
                            r=move || match game.get().state.checkers[i].pos { Position::Prison { .. } | Position::Captured { .. } => 0.22, Position::Reserve => 0.3, _ => 0.36 }
                            class=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Captured { .. }) { "piece captive" } else { "piece" }
                            fill=move || side_color(game.get().state.checkers[i].owner)
                            cx=move || anim_xy(&game.get().state, i, &anim_pts.get()).0
                            cy=move || anim_xy(&game.get().state, i, &anim_pts.get()).1
                            opacity=move || if anim_xy(&game.get().state, i, &anim_pts.get()).2 { 1.0 } else { 0.0 }
                        />
                    </For>
                    <For each=move || stack_counts(&game.get().state) key=|b| b.key let:b>
                        {badge_view(game, b.key)}
                    </For>
                </svg>
                </div>
                </div>
            })}

            // Игровой экран после старта.
            {move || (started.get() && !rules.get()).then(|| view! {
            <div class="status">
                <div class="status-left">
                <button class="icon-btn" title=t_string!(i18n, game_stop) on:click=to_settings>"⏹"</button>
                <button class="icon-btn" class:on=move || paused.get()
                    title=move || if paused.get() { t_string!(i18n, game_resume) } else { t_string!(i18n, game_pause) }
                    on:click=move |_| paused.update(|p| *p = !*p)>
                    {move || if paused.get() { "▶" } else { "⏸" }}
                </button>
                <button class="icon-btn hamburger-btn" class:on=move || hamburger.get()
                    title=t_string!(i18n, hamburger_menu) on:click=move |_| hamburger.update(|h| *h = !*h)>"☰"</button>
                <div class="hamburger-content" class:open=move || hamburger.get()>
                <button class="hamburger-item" on:click=move |_| { rules.set(true); hamburger.set(false); }>"📖" <span class="hamburger-label">{t_string!(i18n, hamburger_rules)}</span></button>
                <button class="hamburger-item" on:click=move |_| { keys_hint.update(|v| *v = !*v); hamburger.set(false); }>"⌨" <span class="hamburger-label">{t_string!(i18n, hamburger_keys)}</span></button>
                <button class="hamburger-item" on:click=move |_| sound.update(|s| *s = !*s)>
                    {move || if sound.get() { "🔊" } else { "🔇" }}<span class="hamburger-label">{t_string!(i18n, hamburger_sound)}</span>
                </button>
                <div class="hamburger-item-wrap">
                <button class="hamburger-item hamburger-locale-btn" on:click=move |_| locale_menu.update(|o| *o = !*o)>
                    "🌐" <span class="hamburger-label">{t_string!(i18n, settings_lang)}</span>
                </button>
                {move || locale_menu.get().then(|| view! {
                    <div class="hamburger-submenu">
                        <button class:on=move || i18n.get_locale() == Locale::ru
                            on:click=move |_| {
                                let _ = web_sys::window().and_then(|w| w.local_storage().ok().flatten().map(|s| s.set_item("locale", "ru").ok()));
                                i18n.set_locale(Locale::ru);
                                locale_menu.set(false);
                            }>"Русский"</button>
                        <button class:on=move || i18n.get_locale() == Locale::en
                            on:click=move |_| {
                                let _ = web_sys::window().and_then(|w| w.local_storage().ok().flatten().map(|s| s.set_item("locale", "en").ok()));
                                i18n.set_locale(Locale::en);
                                locale_menu.set(false);
                            }>"English"</button>
                    </div>
                })}
                </div>
                <div class="hamburger-item-wrap">
                <button class="hamburger-item hamburger-speed-menu" on:click=move |_| speed_menu.update(|o| *o = !*o)>
                    "⏩" <span class="hamburger-label">{t_string!(i18n, hamburger_speed)}</span>
                </button>
                {move || speed_menu.get().then(|| view! {
                    <div class="hamburger-submenu hamburger-speed-menu">
                        <SpeedControl speed i18n=i18n/>
                    </div>
                })}
                </div>
                <div class="hamburger-item-wrap">
                <button class="hamburger-item hamburger-theme-menu" on:click=move |_| theme_menu.update(|o| *o = !*o)>
                    "🎨" <span class="hamburger-label">{t_string!(i18n, hamburger_theme)}</span>
                </button>
                {move || theme_menu.get().then(|| view! {
                    <div class="hamburger-submenu hamburger-theme-menu">
                        {Theme::ALL.into_iter().map(|t| {
                            let on = move || theme.get() == t;
                            view! {
                                <button class:on=on on:click=move |_| theme.set(t)>
                                    {move || match t {
                                        Theme::Midnight => t_string!(i18n, theme_midnight),
                                        Theme::Dusk => t_string!(i18n, theme_dusk),
                                        Theme::Daylight => t_string!(i18n, theme_daylight),
                                        Theme::Forest => t_string!(i18n, theme_forest),
                                    }}
                                </button>
                            }
                        }).collect_view()}
                    </div>
                })}
                </div>
                {cfg!(debug_assertions).then(|| view! {
                    <button class="hamburger-item" on:click=move |_| { show_log.update(|v| *v = !*v); hamburger.set(false); }>"📋" <span class="hamburger-label">{t_string!(i18n, hamburger_log)}</span></button>
                })}
                </div>
                </div>
                <span class="herald" inner_html=move || herald.get()></span>
            </div>

            // Подсказка с горячими клавишами хода.
            {move || keys_hint.get().then(|| view! {
                <div class="keys-hint">
                    <div class="logbox-head">
                        <b>{t_string!(i18n, game_keys_title)}</b>
                        <span class="lead"></span>
                        <button class="icon-btn" title=t_string!(i18n, game_log_close) on:click=move |_| keys_hint.set(false)>"✕"</button>
                    </div>
                    <div class="keys-row"><kbd>"Tab"</kbd><span>{t_string!(i18n, game_keys_tab)}</span></div>
                    <div class="keys-row"><kbd>"1"</kbd><kbd>"2"</kbd><span>{t_string!(i18n, game_keys_1_2)}</span></div>
                    <div class="keys-row"><kbd>"3"</kbd><span>{t_string!(i18n, game_keys_3)}</span></div>
                    <div class="keys-row"><kbd>"i"</kbd><span>{t_string!(i18n, game_keys_i)}</span></div>
                    <div class="keys-row"><kbd>"r"</kbd><span>{t_string!(i18n, game_keys_r)}</span></div>
                </div>
            })}

            // Панель технического лога: текст в textarea (клик — выделить всё, затем Ctrl+C).
            {move || show_log.get().then(|| {
                let log_ref = NodeRef::<leptos::html::Textarea>::new();
                view! {
                    <div class="logbox">
                        <div class="logbox-head">
                            <b>{t_string!(i18n, game_log_title)}</b>
                            <span class="lead">{t_string!(i18n, game_log_hint)}</span>
                            <button class="icon-btn" title=t_string!(i18n, game_log_close) on:click=move |_| show_log.set(false)>"✕"</button>
                        </div>
                        <textarea node_ref=log_ref readonly
                            on:click=move |_| { if let Some(t) = log_ref.get() { t.select(); } }
                        >{dbg_log_dump()}</textarea>
                    </div>
                }
            })}

            <div class="board-area">
            <div class="board-wrap">
            // viewBox с запасом RESERVE_PAD по краям: снаружи доски, напротив каждого
            // Дома, размещены резерв, плен и (у ходящей стороны) кости.
            <svg class="board" viewBox=format!(
                "{o} {o} {s} {s}",
                o = BOARD_MARGIN as f64 - RESERVE_PAD,
                s = (BOARD_DIM - 2 * BOARD_MARGIN) as f64 + 2.0 * RESERVE_PAD,
            )>
                {move || static_board(&game.get().state)}

                <For each=move || 0..game.get().state.checkers.len() key=|i| *i let:i>
                    <circle
                        r=move || match game.get().state.checkers[i].pos { Position::Prison { .. } | Position::Captured { .. } => 0.22, Position::Reserve => 0.3, _ => 0.36 }
                        class=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Captured { .. }) {
                            "piece captive"
                        } else {
                            "piece"
                        }
                        fill=move || side_color(game.get().state.checkers[i].owner)
                        cx=move || anim_xy(&game.get().state, i, &anim_pts.get()).0
                        cy=move || anim_xy(&game.get().state, i, &anim_pts.get()).1
                        opacity=move || if anim_xy(&game.get().state, i, &anim_pts.get()).2 { 1.0 } else { 0.0 }
                    />
                </For>

                // Счётчики одноцветных стопок (Тюрьма, углы, Луна) — keyed, чтобы
                // цифра ехала вместе с кружком (CSS-переход `transform`), а не прыгала.
                <For each=move || stack_counts(&game.get().state) key=|b| b.key let:b>
                    {badge_view(game, b.key)}
                </For>

                {move || {
                    let g = game.get();
                    let state = &g.state;
                    let tm = teams.get_untracked();
                    let total_h = 19.0;
                    let t = 150.0;
                    let mut y = 3.0f64;

                    if tm && state.active.len() == 4 {
                        let team_a = (Heuristic.value(state, Side::A) + Heuristic.value(state, Side::C)) / 2.0;
                        let team_b = (Heuristic.value(state, Side::B) + Heuristic.value(state, Side::D)) / 2.0;
                        let max_v = team_a.max(team_b);
                        let ea = ((team_a - max_v) / t).exp();
                        let eb = ((team_b - max_v) / t).exp();
                        let s = ea + eb;
                        let bar_x = -0.2;
                        let half_w = 0.35 / 2.0;
                        let ha = total_h * (ea / s) as f64;
                        let hb = total_h * (eb / s) as f64;
                        vec![
                            view! { <rect x=bar_x y=y width=half_w height=ha fill="#22d3ee" rx="0.06" /> }.into_any(),
                            view! { <rect x=bar_x + half_w y=y width=half_w height=ha fill="#e879f9" rx="0.06" /> }.into_any(),
                            view! { <rect x=bar_x y=y + ha width=half_w height=hb fill="#4ade80" rx="0.06" /> }.into_any(),
                            view! { <rect x=bar_x + half_w y=y + ha width=half_w height=hb fill="#facc15" rx="0.06" /> }.into_any(),
                        ]
                    } else {
                        let sides: Vec<Side> = state.active.iter().filter(|&&s| !state.has_won(s)).copied().collect();
                        if sides.is_empty() { return vec![]; }
                        let values: Vec<f32> = sides.iter().map(|&s| Heuristic.value(state, s)).collect();
                        let max_v = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                        let exps: Vec<f32> = values.iter().map(|v| ((v - max_v) / t).exp()).collect();
                        let sum_e: f32 = exps.iter().sum();
                        sides.into_iter().zip(exps).flat_map(|(s, e)| {
                            let h = total_h * (e / sum_e) as f64;
                            let col = side_color(s);
                            let y0 = y;
                            y += h;
                            vec![
                                view! { <rect x=-0.2 y=y0 width=0.35 height=h fill=col rx="0.06" /> }.into_any(),
                            ]
                        }).collect::<Vec<_>>()
                    }
                }}

                {move || {
                    let g = game.get();
                    let pre = prefix.get();
                    let ps = g.state.clone(); // доска уже продвинута применёнными частями
                    let mover = g.state.to_move;
                    let active = !animating.get()
                        && roll.get().is_some()
                        && !game_over(&g.state, teams.get())
                        && humans.get().contains(&mover);
                    let all_turns = turns.get();
                    let cands = if active { step_opts(&all_turns, &pre) } else { Vec::new() };
                    let cur = sel.get();

                    // Шаг `m` — «вынужденный промежуточный», если ПОСЛЕ него остаток хода
                    // доигрывается ТОЛЬКО той же фишкой (все продолжения — она же; нет развилки
                    // на другую фишку/ввод). На таком шаге остановиться нельзя (правило
                    // максимума заставит доиграть), поэтому отдельной целью его не показываем —
                    // полный ход даст combo-цель (или объединённый ход). Напр. 6-4 фишкой c0:
                    // «6» ведёт только к «+4», его прячем; «+4» оставляем — после него можно
                    // ввести фишку, это РАЗВИЛКА (реальная точка остановки).
                    let is_forced_intermediate = |m: Move| -> bool {
                        let mut after = pre.clone();
                        after.push(m);
                        let conts = step_opts(&all_turns, &after);
                        !conts.is_empty() && conts.iter().all(|c| c.checker == m.checker)
                    };

                    let mut srcs: Vec<(usize, usize)> = Vec::new();
                    let mut dsts: Vec<(usize, usize)> = Vec::new();
                    for &m in &cands {
                        if let Sel::Cell(r, c) = move_source(&ps, m)
                            && !srcs.contains(&(r, c))
                        {
                            srcs.push((r, c));
                        }
                        if let Some(s) = cur
                            && move_source(&ps, m) == s
                            && !is_forced_intermediate(m)
                            && let Sel::Cell(r, c) = move_dest(&ps, m)
                            && !dsts.contains(&(r, c))
                        {
                            dsts.push((r, c));
                        }
                    }
                    // «Ход сразу за обе кости» одной фишкой — конечные клетки как
                    // дополнительные цели (например, 1-1 → пройти Тюрьму насквозь на 2,
                    // или 1-3 на Луне → сразу на поле «6»). Точку подсветки берём по
                    // РЕАЛЬНОМУ конечному положению фишки (для Луны — на дуге), а не по
                    // центру клетки-сетки, иначе цель «улетала» бы с дуги Луны.
                    let combo_dsts: Vec<((usize, usize), (f64, f64))> = cur
                        .map(|s| combo_targets(&ps, &all_turns, &pre, s))
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|(d, seq)| match d {
                            Sel::Cell(r, c) if !dsts.contains(&(r, c)) => {
                                let ci = seq[0].checker;
                                let mut st = ps.clone();
                                for &m in &seq {
                                    st = apply(&st, m);
                                }
                                let (x, y, _) = checker_xy(&st, ci);
                                Some(((r, c), (x, y)))
                            }
                            _ => None,
                        })
                        .collect();
                    let sel_cell = match cur { Some(Sel::Cell(r, c)) => Some((r, c)), _ => None };
                    // Источник «в каземате» (настоящий пленник) рисуем рамкой вокруг
                    // каземата; фишку на клетке/выходе (в т.ч. уже освобождённую) — кружком
                    // в её НАСТОЯЩЕЙ точке (по центру клетки), а не в каземате.
                    let caged = |rc: (usize, usize)| {
                        cands.iter().any(|&m| {
                            move_source(&ps, m) == Sel::Cell(rc.0, rc.1)
                                && matches!(ps.checkers[m.checker].pos, Position::Prison { .. })
                        })
                    };
                    // Точка подсветки/хита источника — на самой фишке (Луна — на дуге,
                    // угол — со смещением), а не в центре клетки.
                    let src_point = |rc: (usize, usize)| -> (f64, f64) {
                        cands
                            .iter()
                            .find(|&&m| move_source(&ps, m) == Sel::Cell(rc.0, rc.1))
                            .map_or_else(
                                || center_pt(rc),
                                |&m| {
                                    let (x, y, _) = checker_xy(&ps, m.checker);
                                    (x, y)
                                },
                            )
                    };
                    // Точка цели — куда ВСТАНЕТ фишка (Луна — на дугу), кроме входа в
                    // Тюрьму/на Луну, где цель — клетка-ворота (центр клетки периметра).
                    let dst_point = |rc: (usize, usize)| -> (f64, f64) {
                        cands
                            .iter()
                            .find(|&&m| {
                                cur == Some(move_source(&ps, m)) && move_dest(&ps, m) == Sel::Cell(rc.0, rc.1)
                            })
                            .map_or_else(
                                || center_pt(rc),
                                |&m| {
                                    if matches!(m.kind, MoveKind::EnterMoon | MoveKind::EnterPrison) {
                                        center_pt(rc)
                                    } else {
                                        let (x, y, _) = checker_xy(&apply(&ps, m), m.checker);
                                        (x, y)
                                    }
                                },
                            )
                    };

                    let mut nodes: Vec<AnyView> = Vec::new();
                    let ring_pt = |cx: f64, cy: f64, cls: &'static str| {
                        view! { <circle cx=cx cy=cy r=0.47 fill="none" class=cls /> }.into_any()
                    };
                    let cage_ring = |rc: (usize, usize), cls: &'static str| {
                        let (x, y, w, h) = cage_rect(rc).expect("cage rect");
                        view! { <rect x=x y=y width=w height=h rx=0.25 ry=0.25 fill="none" class=cls /> }.into_any()
                    };
                    let pt_hl = |rc: (usize, usize), cls: &'static str| {
                        if caged(rc) {
                            cage_ring(rc, cls)
                        } else {
                            let (cx, cy) = src_point(rc);
                            ring_pt(cx, cy, cls)
                        }
                    };
                    if let Some(rc) = sel_cell {
                        nodes.push(pt_hl(rc, "hl-sel"));
                    }
                    // Цели — на точке, куда встанет фишка (Луна — на дугу).
                    for &rc in &dsts {
                        let (cx, cy) = dst_point(rc);
                        nodes.push(ring_pt(cx, cy, "hl-dst"));
                    }
                    // Цель «за обе кости сразу» — пунктиром, чтобы отличать от одиночной.
                    for &(_, (cx, cy)) in &combo_dsts {
                        nodes.push(ring_pt(cx, cy, "hl-combo"));
                    }
                    // Жёлтые круги вариантов-источников показываем ВСЕГДА (даже когда фишка
                    // уже выбрана) — кроме самой выбранной (она с рамкой `hl-sel`), чтобы
                    // было видно, на какую другую фишку можно переключиться.
                    for &rc in &srcs {
                        if sel_cell != Some(rc) {
                            nodes.push(pt_hl(rc, "hl-src"));
                        }
                    }
                    // Кликабельные зоны: каземат — по рамке И по самой клетке Тюрьмы;
                    // остальное — по центру клетки. Цели — по центру клетки.
                    let mut src_hits: Vec<(usize, usize)> = srcs.clone();
                    if let Some(rc) = sel_cell { src_hits.push(rc); }
                    for rc in src_hits {
                        let (cx, cy) = src_point(rc);
                        nodes.push(view! {
                            <circle cx=cx cy=cy r=0.5 class="hit" on:click=move |_| click(Sel::Cell(rc.0, rc.1)) />
                        }.into_any());
                        if caged(rc) && let Some((x, y, w, h)) = cage_rect(rc) {
                            nodes.push(view! {
                                <rect x=x y=y width=w height=h class="hit" on:click=move |_| click(Sel::Cell(rc.0, rc.1)) />
                            }.into_any());
                        }
                    }
                    for &(r, c) in &dsts {
                        let (cx, cy) = dst_point((r, c));
                        nodes.push(view! {
                            <circle cx=cx cy=cy r=0.5 class="hit" on:click=move |_| click(Sel::Cell(r, c)) />
                        }.into_any());
                    }
                    for &((r, c), (cx, cy)) in &combo_dsts {
                        nodes.push(view! {
                            <circle cx=cx cy=cy r=0.5 class="hit" on:click=move |_| click(Sel::Cell(r, c)) />
                        }.into_any());
                    }
                    // Выносные зоны хода (резерв у своего Дома, плен — у Дома
                    // захватчика): подсветка + клик в их центре.
                    // Полурамка вокруг выносного ряда резерва (4 слота + запас): вдоль
                    // стороны — длинная ось, поперёк — короткая.
                    let res_half = |side: Side| -> (f64, f64) {
                        let (_, along) = reserve_axes(side);
                        let (long, short) = (1.5 * RESERVE_GAP + 0.42, 0.42);
                        if along.0.abs() > along.1.abs() { (long, short) } else { (short, long) }
                    };
                    let mut zone = |kind: Sel, side: Side, out_dist: f64, boxed: bool| {
                        let (zx, zy) = outside_anchor(side, out_dist);
                        let is_src = cands.iter().any(|&m| move_source(&ps, m) == kind);
                        let is_dst = cur.is_some_and(|s| {
                            cands.iter().any(|&m| move_source(&ps, m) == s && move_dest(&ps, m) == kind)
                        });
                        let is_sel = cur == Some(kind);
                        let cls = if is_sel {
                            Some("hl-sel")
                        } else if is_dst {
                            Some("hl-dst")
                        } else if is_src {
                            // Источник подсвечиваем всегда (жёлтым), даже при выбранной
                            // другой фишке — чтобы было видно вариант переключения.
                            Some("hl-src")
                        } else {
                            None
                        };
                        // КЛИКАБЕЛЬНА зона, пока она источник/цель/выбрана — даже когда
                        // выбрана ДРУГАЯ фишка (иначе на резерв не кликнуть, не сняв фокус).
                        // Подсветку же (рамку) рисуем только при наличии класса.
                        let interactive = is_sel || is_dst || is_src;
                        if !interactive {
                            return;
                        }
                        if boxed {
                            // Резерв обводим скруглённой рамкой по всему ряду (не кружком).
                            let (hw, hh) = res_half(side);
                            let (x, y, w, h) = (zx - hw, zy - hh, 2.0 * hw, 2.0 * hh);
                            if let Some(cls) = cls {
                                nodes.push(view! {
                                    <rect x=x y=y width=w height=h rx=0.28 ry=0.28 fill="none" class=cls />
                                }.into_any());
                            }
                            nodes.push(view! {
                                <rect x=x y=y width=w height=h class="hit" on:click=move |_| click(kind) />
                            }.into_any());
                        } else {
                            if let Some(cls) = cls {
                                nodes.push(ring_pt(zx, zy, cls));
                            }
                            nodes.push(view! {
                                <circle cx=zx cy=zy r=1.1 class="hit" on:click=move |_| click(kind) />
                            }.into_any());
                        }
                    };
                    zone(Sel::Reserve, mover, RESERVE_OUT, true);
                    // Пленные хода — у Домов захватчиков; зона на каждом таком Доме.
                    let mut captors: Vec<Side> = Vec::new();
                    for c in &ps.checkers {
                        if c.owner == mover
                            && let Position::Captured { captor } = c.pos
                            && !captors.contains(&captor)
                        {
                            captors.push(captor);
                        }
                    }
                    for captor in captors {
                        zone(Sel::Captured, captor, CAPTURED_OUT, false);
                    }
                    nodes
                }}
                // Выбор человеком обязательного хода на «6» после выкупа компьютером:
                // подсвечиваем каждую фишку захватчика, способную сходить на 6, и её
                // клетку-цель; клик по цели играет этот ход.
                {move || forced_pick.get().map(|(_side, captor, _r, _rem)| {
                    let st = game.get().state;
                    let cur = sel.get(); // выбранный с клавиатуры вариант (Tab) — белой рамкой
                    let mut nodes: Vec<AnyView> = Vec::new();
                    if !animating.get() {
                        let mut shown: Vec<usize> = Vec::new();
                        for mv in forced_six_moves(&st, captor) {
                            if shown.contains(&mv.checker) {
                                continue;
                            }
                            shown.push(mv.checker);
                            let (sx, sy, _) = checker_xy(&st, mv.checker);
                            let src_cls = if cur == Some(move_source(&st, mv)) { "hl-sel" } else { "hl-src" };
                            nodes.push(view! { <circle cx=sx cy=sy r=0.47 fill="none" class=src_cls /> }.into_any());
                            let (tx, ty, _) = checker_xy(&apply(&st, mv), mv.checker);
                            nodes.push(view! { <circle cx=tx cy=ty r=0.47 fill="none" class="hl-dst" /> }.into_any());
                            nodes.push(view! { <circle cx=tx cy=ty r=0.5 class="hit" on:click=move |_| forced_apply(mv) /> }.into_any());
                        }
                    }
                    nodes
                })}
            </svg>
            // Кости — HTML-оверлей над доской, у Дома ходящей стороны (дальше плена).
            {move || {
                let g = game.get();
                let to_move = g.state.to_move;
                let spinning = rolling.get();
                roll.get().map(|r| {
                    let (ax, ay) = outside_anchor(to_move, DICE_OUT);
                    let (out, _) = reserve_axes(to_move);
                    let vertical = out.0 != 0.0; // Дом на левой/правой стороне доски
                    let vb_o = BOARD_MARGIN as f64 - RESERVE_PAD;
                    let vb_s = (BOARD_DIM - 2 * BOARD_MARGIN) as f64 + 2.0 * RESERVE_PAD;
                    let fx = (ax - vb_o) / vb_s * 100.0;
                    let fy = (ay - vb_o) / vb_s * 100.0;
                    let [a, b] = r.values();
                    let inner = if spinning {
                        // Пока кости крутятся, дубль золотом НЕ подсвечиваем — только
                        // после остановки (иначе результат «виден» раньше времени).
                        view! { <span class="dice rolling">{die3d(a)} {die3d(b)}</span> }.into_any()
                    } else {
                        let cls = if r.is_double() { "dice double" } else { "dice" };
                        view! { <span class=cls>{die_face(a)} {die_face(b)}</span> }.into_any()
                    };
                    view! {
                        <div class="board-dice" class:vert=vertical style=format!("left:{fx:.2}%;top:{fy:.2}%")>{inner}</div>
                    }
                })
            }}
            </div>
            </div>
            // Плашка завершения игры: кнопки «Играть снова» / «Главное меню» по центру.
            {move || {
                let g = game.get();
                if game_over(&g.state, teams.get_untracked()) && !animating.get() {
                    view! {
                        <div class="game-over-overlay">
                            <div class="complete-buttons">
                                <button on:click=start_game>
                                    {t_string!(i18n, game_over_play_again)}
                                </button>
                                <button on:click=to_settings>
                                    {t_string!(i18n, game_over_back)}
                                </button>
                            </div>
                        </div>
                    }.into_any()
                } else {
                    ().into_any()
                }
            }}
            })}
        </div>
    }
}
