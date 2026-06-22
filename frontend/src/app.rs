use crate::demo::*;
use crate::geom::*;
use crate::lessons::*;
use crate::model::*;
use crate::moves_ui::*;
use crate::util::*;
use crate::view::*;
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use sheshbesh::board::LOCAL_MOON;
use sheshbesh::moves::forced_six_moves;
use sheshbesh::{
    Agent, BOARD_DIM, BOARD_MARGIN, DiceRoll, DiceSource, Die, Game, GameState, Heuristic, Move,
    MoveKind, Position, RandomDice, Side, apply, best_forced, best_turn, legal_turns, margin_coord,
};
use wasm_bindgen_futures::spawn_local;

#[component]
pub(crate) fn App() -> impl IntoView {
    let dice = StoredValue::new(RandomDice::from_seed(seed()));
    // Поколение партии: растёт при старте/остановке игры. Любая запущенная анимация
    // (spawn_local) запоминает своё поколение и прекращается, когда оно устарело, —
    // иначе старая партия продолжала бы крутить кадры поверх новой.
    let epoch = StoredValue::new(0u64);
    // Настройки: число игроков, типы сторон и (для 4) командный режим 2×2.
    let players = RwSignal::new(2usize);
    let humans = RwSignal::new(vec![Side::A]);
    let teams = RwSignal::new(false);
    let started = RwSignal::new(false);
    // Режим разработчика: экран демонстрации состояний и анимаций (вне партии).
    let dev = RwSignal::new(false);
    // Справка по правилам и пошаговое обучение (туториал) — экраны из меню настроек.
    let rules = RwSignal::new(false);
    let about = RwSignal::new(false);
    let tutorial = RwSignal::new(false);
    let lesson_idx = RwSignal::new(0usize);
    let lessons_sv = StoredValue::new(lessons());
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
    // Состояние на начало текущего хода человека (нужно для доигровки конца хода:
    // вынужденного ответа выкупа и передачи очереди), пока доска уже продвинута.
    let turn_start = StoredValue::new(game.get_untracked().state.clone());
    // Идёт ли сейчас проигрывание анимации хода (блокирует ввод и авто-бросок).
    let animating = RwSignal::new(false);
    // Крутятся ли сейчас кости (анимация броска).
    let rolling = RwSignal::new(false);
    // Покадровое переопределение точек фишек (для движения по параболе Луны).
    let anim_pts = RwSignal::new(Vec::<(usize, f64, f64)>::new());
    // Текст ведущего-комментатора над доской.
    let herald = RwSignal::new(String::new());

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
                // Показываем кадр, затем держим его свою паузу (бросок — дольше шага).
                game.update(|g| g.state = frame.state);
                roll.set(frame.roll);
                rolling.set(frame.rolling);
                anim_pts.set(frame.pts);
                if let Some(note) = frame.note {
                    herald.set(note);
                }
                TimeoutFuture::new(frame.hold).await;
            }
            if epoch.get_value() != era {
                return;
            }
            anim_pts.set(Vec::new());
            rolling.set(false);
            animating.set(false);
        });
    };

    // Автоход компьютерных сторон: по одному ходу за срабатывание Effect. Цепочка
    // ходов «сама себя» продолжает через сигнал `animating` (после анимации хода он
    // снимается → Effect срабатывает снова). Пауза/победа/ход человека её прерывают.
    Effect::new(move |_| {
        let g = game.get();
        let hs = humans.get_untracked();
        if !started.get()
            || animating.get()
            || paused.get()
            || game_over(&g.state, teams.get_untracked())
            || hs.contains(&g.state.to_move)
        {
            return;
        }
        let mut gg = g.clone();
        let mut roll_v = None;
        dice.update_value(|d| roll_v = Some(d.roll()));
        let r = roll_v.expect("roll");
        let t = legal_turns(&gg.state, r);
        // Модель под текущий режим (число сторон + команды).
        let m = ai_model_for(gg.state.active.len(), teams.get_untracked());
        let idx = best_turn(&m, &gg.state, &t).min(t.len() - 1);
        let played = t[idx].clone();
        let mut frames = Vec::new();
        commit_frames(&mut frames, &mut gg, r, played, &hs, true, |s, c, o| {
            best_forced(&m, s, c, o)
        });
        frames.push(Frame {
            state: gg.state.clone(),
            roll: None,
            hold: HOLD_STEP_MS,
            rolling: false,
            note: None,
            pts: Vec::new(),
        });
        play(frames);
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
        let pause = paused.get();
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
            let name = side_name(g.state.to_move, &hs);
            // Анимируем бросок игрока так же, как у компьютера: кубики кувыркаются,
            // затем показывается результат и включается выбор хода.
            let era = epoch.get_value();
            animating.set(true);
            spawn_local(async move {
                herald.set(format!("{name} бросает кости…"));
                roll.set(Some(r));
                rolling.set(true);
                TimeoutFuture::new(ROLL_ANIM_MS).await;
                // Новая партия началась за время броска — прекращаем.
                if epoch.get_value() != era {
                    return;
                }
                rolling.set(false);
                if no_move {
                    // Ходить нечем: показываем бросок и САМИ пропускаем ход через паузу
                    // (без кнопки) — как это делает компьютер.
                    herald.set(format!("{name}: выпало {a} и {b}. Ходить нечем — пропуск."));
                    TimeoutFuture::new(HOLD_NOMOVE_MS).await;
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
                    }]);
                    return;
                }
                herald.set(if r.is_double() {
                    format!("{name}: выпало {a} и {b}. Дубль — ещё ход!")
                } else {
                    format!("{name}: выпало {a} и {b}. Выберите ход.")
                });
                turns.set(t);
                prefix.set(Vec::new());
                sel.set(None);
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
        let mut scratch = after;
        for &fm in &outcome.forced {
            scratch = apply_with_frames(&mut frames, scratch, fm, r, &hs);
        }
        let _ = scratch;
        frames.push(Frame {
            state: fg.state.clone(),
            roll: None,
            hold: HOLD_STEP_MS,
            rolling: false,
            note: None,
            pts: Vec::new(),
        });
        turns.set(Vec::new());
        prefix.set(Vec::new());
        sel.set(None);
        play(frames);
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
        let r = roll.get_untracked().expect("roll");
        let ps = g.state.clone(); // доска уже продвинута применёнными частями хода
        let pre = prefix.get_untracked();
        let cands = step_opts(&turns.get_untracked(), &pre);
        match sel.get_untracked() {
            None => {
                if cands.iter().any(|&m| move_source(&ps, m) == target) {
                    sel.set(Some(target));
                }
            }
            Some(src) if src == target => sel.set(None),
            Some(src) => {
                let found = cands
                    .iter()
                    .find(|&&m| move_source(&ps, m) == src && move_dest(&ps, m) == target);
                if let Some(&m) = found {
                    let mut np = pre.clone();
                    np.push(m);
                    let total = turns.get_untracked().first().map_or(0, Vec::len);
                    // Выбранную часть хода проигрываем сразу — пошагово, по клеткам.
                    let mut frames = Vec::new();
                    let after = apply_with_frames(&mut frames, ps.clone(), m, r, &hs);
                    if np.len() >= total {
                        finish(frames, after, np);
                    } else {
                        // Сохраняем фокус на той же фишке, если ею можно ходить дальше.
                        let next_src = sel_of(
                            after.checkers[m.checker].owner,
                            after.checkers[m.checker].pos,
                        );
                        let keep = step_opts(&turns.get_untracked(), &np)
                            .iter()
                            .any(|&mv| move_source(&after, mv) == next_src);
                        prefix.set(np);
                        sel.set(keep.then_some(next_src));
                        play(frames);
                    }
                } else if cands.iter().any(|&m| move_source(&ps, m) == target) {
                    sel.set(Some(target));
                } else {
                    sel.set(None);
                }
            }
        }
    };

    // Запуск партии по выбранным настройкам (число игроков и кто человек).
    let start_game = move |_| {
        // Новое поколение — глушит анимации предыдущей партии (иначе две идут разом).
        epoch.update_value(|e| *e += 1);
        let active = active_for(players.get_untracked());
        dice.update_value(|d| *d = RandomDice::from_seed(seed()));
        roll.set(None);
        turns.set(Vec::new());
        prefix.set(Vec::new());
        sel.set(None);
        finished.set(Vec::new());
        let g = fresh(dice, active, teams.get_untracked());
        let hs = humans.get_untracked();
        herald.set(format!(
            "Игра началась! Ходит {}.",
            side_name(g.state.to_move, &hs)
        ));
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
        });
        frames.push(Frame {
            state: gg.state.clone(),
            roll: None,
            hold: HOLD_STEP_MS,
            rolling: false,
            note: Some("Демо завершено.".to_string()),
            pts: Vec::new(),
        });
        play(frames);
    };

    // Открыть/закрыть экран разработчика, показав состояние-заглушку.
    let open_dev = move |_| {
        epoch.update_value(|e| *e += 1);
        animating.set(false);
        game.set(Game::new(demo_game(Demo::Reserve)));
        roll.set(None);
        herald.set("Режим разработчика: выберите сценарий.".to_string());
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
        <div class="wrap">
            // Заголовок — только на экране настроек; в партии/обучении он лишь крал бы
            // высоту у доски.
            {move || (!started.get() && !dev.get() && !rules.get() && !about.get() && !tutorial.get())
                .then(|| view! { <h1>"Шеш-Беш"</h1> })}
            // Экран настроек до старта партии: число игроков и тип каждой стороны.
            {move || (!started.get() && !dev.get() && !rules.get() && !about.get() && !tutorial.get()).then(|| view! {
                <div class="settings">
                    <div class="set-row">
                        <span>"Игроков:"</span>
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
                                        {format!("Сторона {}", s.letter())}
                                    </b>
                                    <button class:on=is_h on:click=move |_| toggle_human(s)>
                                        {move || if is_h() { "Человек" } else { "Компьютер" }}
                                    </button>
                                </div>
                            }
                        }).collect_view()
                    }}
                    // Режим вчетвером: команды 2×2 (A+C vs B+D) или каждый сам за себя.
                    {move || (players.get() == 4).then(|| view! {
                        <div class="set-row">
                            <span>"Режим:"</span>
                            <button class:on=move || teams.get() on:click=move |_| teams.set(true)>
                                "Команды 2×2"
                            </button>
                            <button class:on=move || !teams.get() on:click=move |_| teams.set(false)>
                                "Каждый сам"
                            </button>
                        </div>
                    })}
                    <button class="primary" on:click=start_game>"Начать игру"</button>
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
                        }>"🎓 Обучение"</button>
                        <button on:click=move |_| rules.set(true)>"📖 Правила"</button>
                        <button on:click=move |_| about.set(true)>"❤️ От автора"</button>
                        // Режим разработчика — только в отладочной сборке.
                        {cfg!(debug_assertions).then(|| view! {
                            <button class="icon-btn" title="Режим разработчика" on:click=open_dev>"🛠"</button>
                        })}
                    </div>
                </div>
            })}

            // Экран правил: структурированный справочник (самобытные правила).
            {move || rules.get().then(|| view! {
                <div class="rules">
                    <div class="page-head">
                        <button class="icon-btn" title="Назад" on:click=move |_| rules.set(false)>"←"</button>
                        <h2>"Шеш-Беш — правила"</h2>
                    </div>
                    <p class="lead">"Самобытный вариант нард. Играют вдвоём или вчетвером; каждый владеет одной стороной квадрата и её Домом."</p>

                    <h3>"Цель"</h3>
                    <p>"Провести все 4 свои фишки полный круг по доске (против часовой стрелки) и завести их в свой Дом. Чужие фишки в Дом попасть не могут. Кто первым собрал Дом — победил. Вчетвером бывает командная игра 2×2 (победа, когда обе стороны команды собрали Дом) или каждый сам за себя."</p>

                    <h3>"Доска"</h3>
                    <ul>
                        <li>"Квадрат; у каждой из 4 сторон — 19 клеток (углы общие для двух сторон)."</li>
                        <li>"Посередине каждой стороны — "<b>"Дом"</b>" из 4 клеток, идущих внутрь квадрата."</li>
                        <li>"Все фишки стартуют в резерве у своего Дома и вводятся в игру."</li>
                    </ul>

                    <h3>"Ход"</h3>
                    <ul>
                        <li>"За ход бросают две кости. Значения можно отнести к двум разным фишкам (по значению на каждую) или объединить на одну."</li>
                        <li>"«Выбросить число» всегда значит значение на "<b>"одной"</b>" кости, а не сумму."</li>
                        <li>"Ввод фишки в игру — по "<b>"6"</b>". На точку входа нельзя поставить вторую свою фишку; чужую там можно съесть."</li>
                        <li>"Дубль даёт право на ещё один полный ход."</li>
                        <li>"Правило максимального хода: из вариантов выбирают тот, что использует больше очков."</li>
                        <li>"Нельзя перепрыгивать занятые клетки — исключение только углы (через занятый угол проходить можно)."</li>
                    </ul>

                    <h3>"Особые клетки"</h3>
                    <ul>
                        <li><b style="color:#93c5fd">"Луна"</b>" (2-я клетка от угла) — короткий путь. Фишка улетает на внутреннюю дорожку полей 1·3·6: с «1» двигает выброс 1, с «3» — 3, с «6» — 6, после чего возвращается на доску ближе к Дому. Луна полностью безопасна. Чужие Луны срезают путь вперёд, а вот своя Луна (она у самого Дома) выводит фишку в начало круга — это второй круг."</li>
                        <li><b>"Тюрьма"</b>" (4-я клетка от угла) — изолирует фишку: выйти можно только выбросив 4 (фишка остаётся на месте). Удобно блокировать чужие фишки. Если фишка попала на Тюрьму этим же ходом и есть вторая кость — можно пройти мимо."</li>
                        <li><b>"Углы"</b>" — общие клетки; на них фишки не едят и через них можно проходить."</li>
                    </ul>

                    <h3>"Съедание, плен и выкуп"</h3>
                    <ul>
                        <li>"Встав на клетку с чужой фишкой, вы её съедаете — она попадает к вам в плен."</li>
                        <li>"Съедания нет на углах, Луне и в Тюрьме."</li>
                        <li>"Чтобы вернуть пленную фишку, нужно выбросить 6 (выкуп), и тогда соперник обязан сделать ответный ход на 6 клеток. Затем нужна ещё одна 6 для повторного ввода."</li>
                    </ul>

                    <h3>"Дом"</h3>
                    <p>"Заход в Дом — ровно по броску, по одной фишке в клетку; перепрыгивать занятые клетки Дома нельзя (но фишка в Доме может идти глубже, освобождая место). Перебор за пределы Дома нелегален. При этом второй круг возможен через Луну: своя Луна стоит у самого Дома, и фишка с неё выходит в начало круга — приходится идти ещё раз."</p>
                </div>
            })}

            // Экран «Об игре»: личная история происхождения этой самобытной игры.
            {move || about.get().then(|| view! {
                <div class="rules">
                    <div class="page-head">
                        <button class="icon-btn" title="Назад" on:click=move |_| about.set(false)>"←"</button>
                        <h2>"От автора"</h2>
                    </div>
                    <p>"Эта настольная игра ещё в 70-е годы была известна среди студентов МИФИ под названием «Шеш-Беш», «Шиш-Беш» или просто «Шишка». Мой отец играл в неё с одногруппниками."</p>
                    <p>"В 90-е годы я был маленьким и в один из дней мы с отцом купили в книжном магазине лист ватмана и он расчертил на нём поле. Мы стали играть вечерами, сидя на ковре. К нам присоединялись его школьные и студенческие друзья. Было здорово."</p>
                    <p>"Вспоминая эти моменты с теплотой, я решил воссоздать игру на компьютере."</p>
                    <p>"Упоминаний этого варианта игры в Интернете мне найти так и не удалось, находил лишь похожую настолку под названием «Шеш-Беш», но выглядит она несколько иначе: поле в виде креста, без Луны и Тюрьмы."</p>
                </div>
            })}

            // Экран обучения: пошаговые уроки с анимацией. Доска и кубик — те же, что
            // в игре (сигнал `game` + `play` + keyed-фишки со скольжением).
            {move || tutorial.get().then(|| {
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
                                    st = apply_with_frames(frames, st, *mv, opp.roll, &[]);
                                }
                                st.to_move = Side::A; // вернули очередь игроку
                                st
                            }
                            None => after.clone(),
                        };
                        let _ = end_state;
                        let mut next_commit = false;
                        if cur + 1 < ls.len() {
                            // Бросок следующего шага — на ЕГО позиции `before` (она же = конец
                            // хода для непрерывных шагов; для отдельных — переход к новой).
                            let nb = ls[cur + 1].before.clone();
                            next_commit = ls[cur + 1].commit;
                            match ls[cur + 1].roll {
                                Some(nr) => {
                                    frames.extend(roll_only_frames(&nb, nr));
                                    // Шаг-выкуп: сразу проигрываем ход соперника (возврат
                                    // пленной фишки в резерв), оставляя игроку лишь выбор
                                    // обязательного ответного хода на 6 (фаза 1).
                                    if next_commit {
                                        let mut s = nb;
                                        for mv in &ls[cur + 1].moves {
                                            s = apply_with_frames(frames, s, *mv, nr, &[]);
                                        }
                                    }
                                }
                                None => frames.push(Frame { state: nb, roll: None, hold: HOLD_STEP_MS, rolling: false, note: None, pts: Vec::new() }),
                            }
                            lesson_idx.set(cur + 1);
                        }
                        // Для шага-выкупа сразу переходим к фазе выбора хода на 6.
                        tut_played.set(usize::from(next_commit));
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
                                    s = apply_with_frames(&mut frames, s, *mv, r, &[]);
                                }
                                tut_sel.set(false);
                                tut_played.set(1); // дальше — выбор хода на 6
                                play(frames);
                            } else {
                                // Фаза 1: ▶ выбирает первый из обязательных ходов на 6.
                                let opts = forced_six_moves(&st, Side::A);
                                let mut frames = Vec::new();
                                let end = match opts.first() {
                                    Some(mv) => apply_with_frames(&mut frames, st, *mv, r, &[]),
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
                                    state = apply_with_frames(&mut frames, state, *mv, r, &[]);
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
                view! {
                    <div class="status">
                        <button class="icon-btn" title="Настройки" on:click=move |_| { epoch.update_value(|e| *e += 1); animating.set(false); rolling.set(false); tutorial.set(false); }>"←"</button>
                        <span class="herald">{move || {
                            let i = lesson_idx.get();
                            lessons_sv.with_value(|ls| format!("Шаг {}/{total}: {}", i + 1, ls[i].title))
                        }}</span>
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
                                r=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Reserve | Position::Captured { .. }) { 0.3 } else { 0.36 }
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
                                    if k < chosen.len() {
                                        let st = game.get().state;
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
                                                    let end = apply_with_frames(&mut frames, st, mv, r, &[]);
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
                    <button class="nav-arrow left" title="Назад"
                        on:click=move |_| {
                            // Назад — мгновенно: ставим позицию и показываем её кости.
                            epoch.update_value(|e| *e += 1);
                            animating.set(false);
                            rolling.set(false);
                            tut_sel.set(false);
                            tut_played.set(0);
                            tut_pick.set(None);
                            let i = lesson_idx.get_untracked().saturating_sub(1);
                            lesson_idx.set(i);
                            lessons_sv.with_value(|ls| {
                                game.set(Game::new(ls[i].before.clone()));
                                roll.set(ls[i].roll);
                            });
                        }>"◀"</button>
                    <button class="nav-arrow right" title="Далее"
                        on:click=move |_| play_submoves(usize::MAX)>"▶"</button>
                    </div>
                    </div>
                    <p class="lesson-text">{move || {
                        // Пока кости кувыркаются — нейтральная заглушка: текст урока
                        // называет выпавшие значения и спойлерил бы ещё не вставший бросок.
                        // Кости противника (ход C) подписываем отдельно.
                        if rolling.get() {
                            if game.get().state.to_move == Side::A {
                                "🎲 Бросаем кости…".to_string()
                            } else {
                                "🎲 Противник бросает кости…".to_string()
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
                                <p class="lesson-hint">"👆 Выберите, какой фишкой сделать обязательный ход на 6: нажмите фишку, затем её клетку."</p>
                            }).into_any()
                        } else if ls[cur].roll.is_some() {
                            view! {
                                <p class="lesson-hint">"👆 Сделайте ход сами: нажмите на фишку, затем на подсвеченную клетку. Или жмите ▶."</p>
                            }.into_any()
                        } else {
                            ().into_any()
                        }
                    })}
                }
            })}

            // Экран разработчика: панель сценариев + демо-анимация + read-only доска.
            {move || dev.get().then(|| view! {
                <div class="status">
                    <button class="icon-btn" title="Настройки" on:click=move |_| { epoch.update_value(|e| *e += 1); animating.set(false); dev.set(false); }>"←"</button>
                    <span class="herald" inner_html=move || herald.get()></span>
                </div>
                <div class="controls dev-controls">
                    {DEMOS.iter().map(|&(d, label)| view! {
                        <button on:click=move |_| {
                            // Обрываем демо-анимацию, если идёт, и показываем сценарий.
                            epoch.update_value(|e| *e += 1);
                            animating.set(false);
                            game.set(Game::new(demo_game(d)));
                            roll.set(None);
                            herald.set(label.to_string());
                        }>{label}</button>
                    }).collect_view()}
                    <button on:click=anim_demo>"▶ Анимация: ход и съедание"</button>
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
                            r=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Reserve | Position::Captured { .. }) { 0.3 } else { 0.36 }
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
            {move || started.get().then(|| view! {
            <div class="status">
                <button class="icon-btn" title="Закончить игру" on:click=to_settings>"⏹"</button>
                <button class="icon-btn" class:on=move || paused.get()
                    title=move || if paused.get() { "Продолжить" } else { "Пауза" }
                    on:click=move |_| paused.update(|p| *p = !*p)>
                    {move || if paused.get() { "▶" } else { "⏸" }}
                </button>
                <span class="herald" inner_html=move || herald.get()></span>
            </div>

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
                        r=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Reserve | Position::Captured { .. }) { 0.3 } else { 0.36 }
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
                    let pre = prefix.get();
                    let ps = g.state.clone(); // доска уже продвинута применёнными частями
                    let mover = g.state.to_move;
                    let active = !animating.get()
                        && roll.get().is_some()
                        && !game_over(&g.state, teams.get())
                        && humans.get().contains(&mover);
                    let cands = if active { step_opts(&turns.get(), &pre) } else { Vec::new() };
                    let cur = sel.get();

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
                            && let Sel::Cell(r, c) = move_dest(&ps, m)
                            && !dsts.contains(&(r, c))
                        {
                            dsts.push((r, c));
                        }
                    }
                    let sel_cell = match cur { Some(Sel::Cell(r, c)) => Some((r, c)), _ => None };
                    let pt_of = |coord: (usize, usize)| {
                        prison_cage(coord).unwrap_or_else(|| center_pt(coord))
                    };

                    let mut nodes: Vec<AnyView> = Vec::new();
                    let ring_pt = |cx: f64, cy: f64, cls: &'static str| {
                        view! { <circle cx=cx cy=cy r=0.47 fill="none" class=cls /> }.into_any()
                    };
                    let ring = |coord: (usize, usize), cls: &'static str| {
                        let (cx, cy) = pt_of(coord);
                        ring_pt(cx, cy, cls)
                    };
                    if let Some(rc) = sel_cell {
                        nodes.push(ring(rc, "hl-sel"));
                    }
                    // Цели рисуем на самой клетке (центр), а НЕ через `pt_of`: для входа в
                    // Тюрьму это клетка периметра, а не каземат.
                    for &rc in &dsts {
                        let (cx, cy) = center_pt(rc);
                        nodes.push(ring_pt(cx, cy, "hl-dst"));
                    }
                    if cur.is_none() {
                        for &rc in &srcs {
                            nodes.push(ring(rc, "hl-src"));
                        }
                    }
                    // Кликабельные зоны: источники/выбранная — в точке фишки (каземат для
                    // пленённых), цели — в центре клетки входа.
                    let mut src_hits: Vec<(usize, usize)> = srcs.clone();
                    if let Some(rc) = sel_cell { src_hits.push(rc); }
                    for (r, c) in src_hits {
                        let (cx, cy) = pt_of((r, c));
                        nodes.push(view! {
                            <circle cx=cx cy=cy r=0.5 class="hit" on:click=move |_| click(Sel::Cell(r, c)) />
                        }.into_any());
                    }
                    for &(r, c) in &dsts {
                        let (cx, cy) = center_pt((r, c));
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
                        let (long, short) = (1.5 * RESERVE_GAP + 0.5, 0.5);
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
                        } else if is_src && cur.is_none() {
                            Some("hl-src")
                        } else {
                            None
                        };
                        let Some(cls) = cls else { return };
                        if boxed {
                            // Резерв обводим скруглённой рамкой по всему ряду (не кружком).
                            let (hw, hh) = res_half(side);
                            let (x, y, w, h) = (zx - hw, zy - hh, 2.0 * hw, 2.0 * hh);
                            nodes.push(view! {
                                <rect x=x y=y width=w height=h rx=0.28 ry=0.28 fill="none" class=cls />
                            }.into_any());
                            nodes.push(view! {
                                <rect x=x y=y width=w height=h class="hit" on:click=move |_| click(kind) />
                            }.into_any());
                        } else {
                            nodes.push(ring_pt(zx, zy, cls));
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
            })}
        </div>
    }
}
