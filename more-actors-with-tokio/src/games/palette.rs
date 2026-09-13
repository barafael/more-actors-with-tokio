//! Per-connection palette: Tableau 10 (colorous), cycling after 10 hosts. A
//! connection's id doubles as its palette slot, so color is stable and
//! consistent across every client and game.

use colorous::TABLEAU10;

pub const PALETTE_LEN: usize = 10;

pub fn color_index(conn: u64) -> usize {
    (conn as usize) % PALETTE_LEN
}

pub fn sender_hex(conn: u64) -> String {
    let c = TABLEAU10[color_index(conn)];
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// White text on dark palette entries, ink on light ones.
pub fn sender_text_color(conn: u64) -> &'static str {
    let c = TABLEAU10[color_index(conn)];
    let lum = (0.299 * f64::from(c.r) + 0.587 * f64::from(c.g) + 0.114 * f64::from(c.b)) / 255.0;
    if lum > 0.62 {
        "#333"
    } else {
        "#fff"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_stable_per_conn_and_cycle() {
        assert_eq!(sender_hex(0), sender_hex(10));
        assert_eq!(color_index(1), 1);
        assert_eq!(color_index(11), 1);
    }
}
