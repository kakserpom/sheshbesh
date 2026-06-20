use sheshbesh::{Game, Heuristic, RandomDice, Side, render};

fn main() {
    // Демонстрация: эвристика играет сама с собой; первый ход разыгрывается костями.
    let mut dice = RandomDice::from_entropy();
    let mut game = Game::start(vec![Side::A, Side::A.opposite()], &mut dice);
    let mut agent = Heuristic;
    println!(
        "Право первого хода: сторона {}",
        game.state.to_move.letter()
    );

    let mut rounds = 0;
    for _ in 0..2000 {
        if game.winner().is_some() {
            break;
        }
        game.play_turn(&mut dice, &mut agent);
        rounds += 1;
    }

    println!("\nДоска после {rounds} ходов:\n");
    println!("{}", render(&game.state));
    match game.winner() {
        Some(side) => println!("\nПобедила сторона {}", side.letter()),
        None => println!("\nЗа {rounds} ходов победитель не определился"),
    }
}
