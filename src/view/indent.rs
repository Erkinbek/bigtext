//! Indent guides for code: the indentation step guessed from the start of a file, and the
//! indentation of blank lines taken from the lines around them.

use bigtext::text::rows::TAB_WIDTH;

/// How much of the start of a file the step is guessed from.
pub const GUESS_BYTES: u64 = 64 * 1024;

/// The indentation step in cells: the tab width when more lines are indented with tabs than
/// with spaces, else the most common step from a line to a deeper next line (2 to 8 cells; one
/// space steps are comment stars and alignment). 4 when nothing tells.
pub fn guess_unit(text: &[u8]) -> u32 {
    let (mut tabs, mut spaced) = (0usize, 0usize);
    let mut steps = [0usize; 9];
    let mut prev: Option<usize> = None;
    for line in text.split(|&b| b == b'\n') {
        let ws = line.iter().take_while(|&&b| b == b' ' || b == b'\t').count();
        if line[ws..].iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if line[0] == b'\t' {
            tabs += 1;
            prev = None;
            continue;
        }
        if ws > 0 {
            spaced += 1;
        }
        if let Some(p) = prev.filter(|&p| ws > p) {
            if let Some(n) = steps.get_mut(ws - p).filter(|_| ws - p >= 2) {
                *n += 1;
            }
        }
        prev = Some(ws);
    }
    if tabs > spaced {
        return TAB_WIDTH as u32;
    }
    let mut unit = (4, 0);
    for (step, &n) in steps.iter().enumerate() {
        if n > unit.1 {
            unit = (step as u32, n);
        }
    }
    unit.0
}

/// The indentation of each row in cells. A blank row (`None`) takes the smaller indentation of
/// the nearest rows above and below it that have text, or the one of them that is known.
pub fn fill_blanks(rows: &[Option<u32>]) -> Vec<u32> {
    let mut below = vec![None; rows.len()];
    let mut next = None;
    for i in (0..rows.len()).rev() {
        below[i] = next;
        if rows[i].is_some() {
            next = rows[i];
        }
    }
    let mut above = None;
    let mut out = Vec::with_capacity(rows.len());
    for (i, &r) in rows.iter().enumerate() {
        out.push(match (r, above, below[i]) {
            (Some(n), _, _) => n,
            (None, Some(a), Some(b)) => u32::min(a, b),
            (None, a, b) => a.or(b).unwrap_or(0),
        });
        if r.is_some() {
            above = r;
        }
    }
    out
}

/// The cells guides are drawn at for a row indented by `indent` cells: one per step.
pub fn guide_cells(indent: u32, unit: u32) -> impl Iterator<Item = u32> {
    (0..indent).step_by(unit.max(1) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_the_step() {
        let py = "class A:\n    def f(self):\n        return 1\n\n    def g(self,\n          x):\n        pass\n";
        assert_eq!(guess_unit(py.as_bytes()), 4);
        let js = "function f() {\n  if (x) {\n    y();\n  }\n}\n/**\n * Docs.\n */\n";
        assert_eq!(guess_unit(js.as_bytes()), 2);
        let go = "func f() {\n\tif x {\n\t\ty()\n\t}\n}\n";
        assert_eq!(guess_unit(go.as_bytes()), TAB_WIDTH as u32);
        assert_eq!(guess_unit(b"no indentation\nat all\n"), 4);
        assert_eq!(guess_unit(b""), 4);
    }

    #[test]
    fn blank_rows_follow_their_neighbours() {
        assert_eq!(fill_blanks(&[Some(4), None, Some(8), None, None, Some(4)]), vec![4, 4, 8, 4, 4, 4]);
        assert_eq!(fill_blanks(&[None, Some(8), None]), vec![8, 8, 8]);
        assert_eq!(fill_blanks(&[None, None]), vec![0, 0]);
        assert_eq!(guide_cells(8, 4).collect::<Vec<_>>(), vec![0, 4]);
        assert_eq!(guide_cells(6, 4).collect::<Vec<_>>(), vec![0, 4]);
        assert_eq!(guide_cells(0, 4).count(), 0);
    }
}
