//! Which word a person probably meant (ADR-025): one rule for every "did you mean" — command words,
//! setting names, values from a closed set (time zones), package names.

/// How far apart two words are: the letters added, removed or changed, two letters swapped counting
/// as one (`shwo` is one from `show`); case ignored.
pub fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.to_lowercase().chars().collect(), b.to_lowercase().chars().collect());
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// The words of `candidates` `w` probably meant, closest first (at most three, all of the closest
/// kind): the same word in another case (`permitrootlogin`), words it begins (`hostn` → hostName),
/// words holding it (`Berlin` → Europe/Berlin), then a slip — one edit in a word of up to four
/// letters, two in a longer one.
pub fn suggest<'a>(w: &str, candidates: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    if w.is_empty() {
        return vec![];
    }
    let lw = w.to_lowercase();
    let room = if w.chars().count() <= 4 { 1 } else { 2 };
    // (kind, distance, word): the lower, the closer.
    let mut found: Vec<(u8, usize, &str)> = candidates
        .into_iter()
        .filter(|c| *c != w && !c.starts_with('<'))
        .filter_map(|c| {
            let lc = c.to_lowercase();
            let kind = if lc == lw {
                0
            } else if lc.starts_with(&lw) {
                1
            } else if lw.chars().count() >= 3 && lc.contains(&lw) {
                2
            } else {
                3
            };
            let d = distance(c, w);
            (kind < 3 || (d <= room && d < w.chars().count())).then_some((kind, d, c))
        })
        .collect();
    found.sort();
    let best = found.first().map(|(k, d, _)| (*k, if *k == 3 { *d } else { usize::MAX }));
    found
        .into_iter()
        .filter(|(k, d, _)| Some((*k, if *k == 3 { *d } else { usize::MAX })) == best)
        .take(3)
        .map(|(_, _, c)| c)
        .collect()
}

/// `; did you mean a, b?` (after the sentence it adds to), or nothing.
pub fn hint<'a>(w: &str, candidates: impl IntoIterator<Item = &'a str>) -> String {
    match suggest(w, candidates).as_slice() {
        [] => String::new(),
        s => format!("; did you mean {}?", s.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_meant() {
        let words = ["show", "set", "hosts", "hostName", "PermitRootLogin", "Europe/Berlin", "America/Berlin_x", "version", "delete"];
        let s = |w: &str| suggest(w, words);
        assert_eq!(s("shwo"), ["show"], "two letters swapped: one slip");
        assert_eq!(s("permitrootlogin"), ["PermitRootLogin"], "case");
        assert_eq!(s("hostn"), ["hostName"], "a beginning, before a slip (hosts)");
        assert_eq!(s("Berlin"), ["Europe/Berlin", "America/Berlin_x"], "holding it, the closer first");
        assert_eq!(s("versoin"), ["version"]);
        assert_eq!(s("delte"), ["delete"]);
        assert_eq!(s("sed"), ["set"], "one letter off a short word");
        assert!(suggest("sed", ["head", "grep"]).is_empty(), "two off a short word is too far");
        assert!(s("zzzz").is_empty());
        assert_eq!(hint("shwo", words), "; did you mean show?");
        assert_eq!(distance("Show", "show"), 0);
    }
}
