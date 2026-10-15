//! The US keyboard layout as HID keyboard usages (usage page 0x07), for typing text into a
//! simulator one key at a time.

/// The HID usage that types `c` on a US layout, and whether it needs Shift; `None` for a
/// character no US key types (it is pasted instead). `\n` and `\r` are Return, `\t` is Tab.
pub(crate) fn us_usage(c: char) -> Option<(u32, bool)> {
    let unshifted = |usage| Some((usage, false));
    let shifted = |usage| Some((usage, true));
    match c {
        'a'..='z' => unshifted(0x04 + (c as u32 - 'a' as u32)),
        'A'..='Z' => shifted(0x04 + (c as u32 - 'A' as u32)),
        '1'..='9' => unshifted(0x1E + (c as u32 - '1' as u32)),
        '0' => unshifted(0x27),
        '!' => shifted(0x1E),
        '@' => shifted(0x1F),
        '#' => shifted(0x20),
        '$' => shifted(0x21),
        '%' => shifted(0x22),
        '^' => shifted(0x23),
        '&' => shifted(0x24),
        '*' => shifted(0x25),
        '(' => shifted(0x26),
        ')' => shifted(0x27),
        '\n' | '\r' => unshifted(0x28),
        '\t' => unshifted(0x2B),
        ' ' => unshifted(0x2C),
        '-' => unshifted(0x2D),
        '_' => shifted(0x2D),
        '=' => unshifted(0x2E),
        '+' => shifted(0x2E),
        '[' => unshifted(0x2F),
        '{' => shifted(0x2F),
        ']' => unshifted(0x30),
        '}' => shifted(0x30),
        '\\' => unshifted(0x31),
        '|' => shifted(0x31),
        ';' => unshifted(0x33),
        ':' => shifted(0x33),
        '\'' => unshifted(0x34),
        '"' => shifted(0x34),
        '`' => unshifted(0x35),
        '~' => shifted(0x35),
        ',' => unshifted(0x36),
        '<' => shifted(0x36),
        '.' => unshifted(0x37),
        '>' => shifted(0x37),
        '/' => unshifted(0x38),
        '?' => shifted(0x38),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_layout_covers_letters_digits_and_shifted_symbols() {
        assert_eq!(us_usage('a'), Some((0x04, false)));
        assert_eq!(us_usage('z'), Some((0x1D, false)));
        assert_eq!(us_usage('A'), Some((0x04, true)));
        assert_eq!(us_usage('1'), Some((0x1E, false)));
        assert_eq!(us_usage('0'), Some((0x27, false)));
        assert_eq!(us_usage('!'), Some((0x1E, true)));
        assert_eq!(us_usage(' '), Some((0x2C, false)));
        assert_eq!(us_usage('?'), Some((0x38, true)));
        assert_eq!(us_usage('\n'), Some((0x28, false)));
        assert_eq!(us_usage('é'), None);
        assert_eq!(us_usage('😀'), None);
    }

    #[test]
    fn every_printable_ascii_character_has_a_key() {
        for c in (0x20u8..0x7F).map(char::from) {
            assert!(us_usage(c).is_some(), "{c:?}");
        }
    }
}
