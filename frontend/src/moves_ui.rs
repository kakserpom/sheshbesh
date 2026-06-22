use crate::util::Sel;
use sheshbesh::board::LOCAL_MOON;
use sheshbesh::{GameState, Move, MoveKind, Position, Side, apply, checker_cell, margin_coord};

/// Уникальные ходы текущего шага среди полных ходов с префиксом `prefix`.
pub(crate) fn step_opts(turns: &[Vec<Move>], prefix: &[Move]) -> Vec<Move> {
    let step = prefix.len();
    let mut out: Vec<Move> = Vec::new();
    for t in turns {
        if t.len() > step && t[..step] == *prefix && !out.contains(&t[step]) {
            out.push(t[step]);
        }
    }
    out
}

pub(crate) fn move_source(ps: &GameState, m: Move) -> Sel {
    sel_of(ps.checkers[m.checker].owner, ps.checkers[m.checker].pos)
}

pub(crate) fn move_dest(ps: &GameState, m: Move) -> Sel {
    let after = apply(ps, m);
    let pos = after.checkers[m.checker].pos;
    // Заход в Тюрьму/на Луну целим в КЛЕТКУ входа (периметр), а не в каземат / на дугу
    // Луны, куда фишка затем уходит, — там подсветка цели выглядела бы не на месте.
    match (m.kind, pos) {
        (MoveKind::EnterPrison, Position::Prison { cell }) => {
            let (r, c) = margin_coord(cell);
            Sel::Cell(r, c)
        }
        (MoveKind::EnterMoon, Position::Moon { side, .. }) => {
            let (r, c) = margin_coord(side.local_to_perimeter(LOCAL_MOON));
            Sel::Cell(r, c)
        }
        _ => sel_of(after.checkers[m.checker].owner, pos),
    }
}

pub(crate) fn sel_of(owner: Side, pos: Position) -> Sel {
    match pos {
        Position::Reserve => Sel::Reserve,
        Position::Captured { .. } => Sel::Captured,
        _ => checker_cell(owner, pos).map_or(Sel::Reserve, |(r, c)| Sel::Cell(r, c)),
    }
}
