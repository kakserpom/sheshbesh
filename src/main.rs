use sheshbesh::{FirstChoice, Game, RandomDice, Side, render};

fn main() {
    // Демонстрация: партия на двоих с розыгрышем первого хода; агент берёт первый ход.
    let mut dice = RandomDice::from_entropy();
    let mut game = Game::start(vec![Side::A, Side::A.opposite()], &mut dice);
    let mut agent = FirstChoice;
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
