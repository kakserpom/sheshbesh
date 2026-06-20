//! Кости и результат броска.
//!
//! За ход игрок кидает две кости. Где правило требует «выбросить» конкретное число
//! (6 — ввод/выкуп, 4 — выход из Тюрьмы, 1/3/6 — Луна), нужно совпадение на
//! **одной** из костей; сумма не засчитывается.

/// Одна шестигранная кость со значением 1..=6.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Die(u8);

impl Die {
    /// Создаёт кость; возвращает `None`, если значение вне диапазона 1..=6.
    pub fn new(value: u8) -> Option<Die> {
        (1..=6).contains(&value).then_some(Die(value))
    }

    /// Значение кости (1..=6).
    pub fn value(self) -> u8 {
        self.0
    }
}

/// Результат броска двух костей.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DiceRoll {
    a: Die,
    b: Die,
}

impl DiceRoll {
    /// Создаёт бросок из двух костей.
    pub fn new(a: Die, b: Die) -> DiceRoll {
        DiceRoll { a, b }
    }

    /// Значения обеих костей.
    pub fn values(self) -> [u8; 2] {
        [self.a.value(), self.b.value()]
    }

    /// Сумма очков (используется при объединении хода на одну фишку).
    pub fn pips(self) -> u8 {
        self.a.value() + self.b.value()
    }

    /// Дубль — даёт право на внеочередной ход.
    pub fn is_double(self) -> bool {
        self.a == self.b
    }

    /// Показала ли хотя бы одна кость значение `value`
    /// (для правил, требующих конкретное число на одной кости).
    pub fn has_value(self, value: u8) -> bool {
        self.a.value() == value || self.b.value() == value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn die(v: u8) -> Die {
        Die::new(v).unwrap()
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(Die::new(0).is_none());
        assert!(Die::new(7).is_none());
        assert!(Die::new(1).is_some());
        assert!(Die::new(6).is_some());
    }

    #[test]
    fn double_and_pips() {
        let roll = DiceRoll::new(die(3), die(3));
        assert!(roll.is_double());
        assert_eq!(roll.pips(), 6);
    }

    #[test]
    fn has_value_checks_single_die_not_sum() {
        let roll = DiceRoll::new(die(2), die(4));
        assert!(roll.has_value(4));
        assert!(roll.has_value(2));
        // Сумма равна 6, но ни одна кость не показывает 6.
        assert!(!roll.has_value(6));
    }
}
