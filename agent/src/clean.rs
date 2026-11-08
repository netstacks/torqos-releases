//! Text as it may reach a terminal, or a model: what a model wrote, or a log held, can carry escape
//! sequences — to clear the screen, retitle the window, rewrite a line already shown (a forged
//! "nothing changed"), or hide text with a bidirectional override. None of it survives here.

/// `s` with every terminal escape sequence (CSI, OSC, DCS and the other strings, any two-character
/// ESC sequence, their 8-bit C1 forms) and every control character removed — newlines and tabs kept,
/// a carriage return dropped — and the bidirectional controls, zero-width characters, the line and
/// paragraph separators and the byte-order mark removed.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\n' | '\t' => out.push(c),
            '\u{1b}' => match it.next() {
                Some('[') => skip_csi(&mut it),
                Some(']' | 'P' | 'X' | '^' | '_') => skip_string(&mut it),
                // ESC, any intermediates, a final character.
                Some(c) if ('\u{20}'..='\u{2f}').contains(&c) => {
                    while it.next_if(|c| ('\u{20}'..='\u{2f}').contains(c)).is_some() {}
                    it.next();
                }
                _ => {}
            },
            '\u{9b}' => skip_csi(&mut it),
            '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => skip_string(&mut it),
            c if c.is_control() => {}
            // Bidirectional overrides, embeddings and isolates; the marks (LRM, RLM, ALM); zero-width and
            // invisible characters; the line and paragraph separators; the byte-order mark.
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}' => {}
            '\u{200b}'..='\u{200d}' | '\u{2060}'..='\u{2064}' | '\u{2028}' | '\u{2029}' | '\u{feff}' => {}
            c => out.push(c),
        }
    }
    out
}

/// A control sequence's parameters and intermediates, through its final character (0x40–0x7e).
fn skip_csi(it: &mut std::iter::Peekable<std::str::Chars>) {
    for c in it.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&c) {
            return;
        }
    }
}

/// A string (OSC, DCS, SOS, PM, APC), through its terminator: BEL, ESC \, or the 8-bit ST.
fn skip_string(it: &mut std::iter::Peekable<std::str::Chars>) {
    while let Some(c) = it.next() {
        match c {
            '\u{7}' | '\u{9c}' => return,
            '\u{1b}' => {
                it.next_if_eq(&'\\');
                return;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn no_escape_reaches_the_terminal() {
        let cases = [
            ("\x1b[2J\x1b[Hok", "ok"),
            ("a\x1b[1;31mred\x1b[0m b", "ared b"),
            ("\x1b]0;evil title\x07x", "x"),
            ("\x1b]8;;http://evil\x1b\\link\x1b]8;;\x1b\\", "link"),
            ("\x1bPq#0;2;0;0;0\x1b\\after", "after"),
            ("\x1b(Bplain", "plain"),
            ("\x1b7saved\x1b8", "saved"),
            ("\u{9b}31mc1\u{9d}0;t\u{9c}!", "c1!"),
            ("line\roverwritten", "lineoverwritten"),
            ("bell\x07 back\x08space nul\0 del\x7f", "bell backspace nul del"),
            ("rtl \u{202e}desrever\u{202c} \u{2066}iso\u{2069}", "rtl desrever iso"),
            ("z\u{200b}e\u{200c}r\u{200d}o\u{2060} \u{feff}bom \u{200e}l\u{200f}r\u{061c}a", "zero bom lra"),
            ("one\u{2028}line\u{2029}", "oneline"),
            ("keep\n\ttabs and lines, ünïcödé ✓", "keep\n\ttabs and lines, ünïcödé ✓"),
            ("unterminated \x1b]0;title", "unterminated "),
            ("trailing \x1b", "trailing "),
        ];
        for (dirty, want) in cases {
            let got = clean(dirty);
            assert_eq!(got, want, "{dirty:?}");
            assert!(!got.chars().any(|c| c.is_control() && c != '\n' && c != '\t'), "{got:?}");
        }
    }
}
