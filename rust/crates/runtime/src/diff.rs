//! Построчный diff (LCS) для превью правок и `structuredPatch`.
//!
//! Раньше и `make_patch`, и TUI-превью показывали «все старые строки, потом
//! все новые» — две версии подряд вместо слияния. Здесь честный построчный
//! diff: общие строки становятся контекстом, изменения перемежаются как в
//! `git diff`. Для патологически больших правок LCS-таблица не строится
//! (потолок сложности) — тогда деградируем до старого блочного вида, что
//! всё ещё корректно, просто грубее.

/// Одна строка диффа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOp<'a> {
    /// Строка есть в обеих версиях.
    Equal(&'a str),
    /// Строка удалена из старой версии.
    Remove(&'a str),
    /// Строка добавлена в новой версии.
    Insert(&'a str),
}

impl LineOp<'_> {
    /// Маркер строки в unified-diff (`' '`/`'-'`/`'+'`).
    #[must_use]
    pub fn marker(&self) -> char {
        match self {
            Self::Equal(_) => ' ',
            Self::Remove(_) => '-',
            Self::Insert(_) => '+',
        }
    }

    /// Текст строки без маркера.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Equal(text) | Self::Remove(text) | Self::Insert(text) => text,
        }
    }
}

/// Потолок на размер LCS-таблицы: изменённая середина до 1000×1000 строк
/// покрывает любые реальные правки; сверх — блочный фолбэк без таблицы.
const MAX_LCS_CELLS: usize = 1_000_000;

/// Построчный diff двух текстов. Общие префикс и суффикс вычисляются за
/// линию, LCS строится только для изменённой середины.
#[must_use]
pub fn line_ops<'a>(old: &'a str, new: &'a str) -> Vec<LineOp<'a>> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let prefix = old_lines
        .iter()
        .zip(new_lines.iter())
        .take_while(|(old, new)| old == new)
        .count();
    let max_suffix = old_lines.len().min(new_lines.len()) - prefix;
    let suffix = old_lines
        .iter()
        .rev()
        .zip(new_lines.iter().rev())
        .take_while(|(old, new)| old == new)
        .take(max_suffix)
        .count();

    let removed = &old_lines[prefix..old_lines.len() - suffix];
    let added = &new_lines[prefix..new_lines.len() - suffix];

    let mut ops = Vec::with_capacity(old_lines.len().max(new_lines.len()));
    ops.extend(old_lines[..prefix].iter().map(|line| LineOp::Equal(line)));
    middle_ops(removed, added, &mut ops);
    ops.extend(
        old_lines[old_lines.len() - suffix..]
            .iter()
            .map(|line| LineOp::Equal(line)),
    );
    ops
}

/// Diff изменённой середины: LCS по таблице, если она влезает в потолок,
/// иначе блочно («все -, потом все +»).
fn middle_ops<'a>(removed: &[&'a str], added: &[&'a str], ops: &mut Vec<LineOp<'a>>) {
    if removed.is_empty() && added.is_empty() {
        return;
    }
    if removed.is_empty() || added.is_empty() || removed.len() * added.len() > MAX_LCS_CELLS {
        ops.extend(removed.iter().map(|line| LineOp::Remove(line)));
        ops.extend(added.iter().map(|line| LineOp::Insert(line)));
        return;
    }

    // Классическая LCS-таблица (n+1)×(m+1); середина после среза
    // префикса/суффикса почти всегда маленькая.
    let n = removed.len();
    let m = added.len();
    let mut table = vec![0u32; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[idx(i, j)] = if removed[i] == added[j] {
                table[idx(i + 1, j + 1)] + 1
            } else {
                table[idx(i + 1, j)].max(table[idx(i, j + 1)])
            };
        }
    }

    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if removed[i] == added[j] {
            ops.push(LineOp::Equal(removed[i]));
            i += 1;
            j += 1;
        } else if table[idx(i + 1, j)] >= table[idx(i, j + 1)] {
            ops.push(LineOp::Remove(removed[i]));
            i += 1;
        } else {
            ops.push(LineOp::Insert(added[j]));
            j += 1;
        }
    }
    ops.extend(removed[i..].iter().map(|line| LineOp::Remove(line)));
    ops.extend(added[j..].iter().map(|line| LineOp::Insert(line)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(ops: &[LineOp<'_>]) -> Vec<String> {
        ops.iter()
            .map(|op| format!("{}{}", op.marker(), op.text()))
            .collect()
    }

    #[test]
    fn interleaves_changes_with_context() {
        let old = "fn main() {\n    let a = 1;\n    println!(\"{a}\");\n}\n";
        let new = "fn main() {\n    let a = 2;\n    println!(\"{a}\");\n}\n";
        assert_eq!(
            render(&line_ops(old, new)),
            vec![
                " fn main() {",
                "-    let a = 1;",
                "+    let a = 2;",
                "     println!(\"{a}\");",
                " }",
            ]
        );
    }

    #[test]
    fn keeps_common_lines_inside_changed_region() {
        let old = "a\nkeep\nb\n";
        let new = "x\nkeep\ny\n";
        assert_eq!(
            render(&line_ops(old, new)),
            vec!["-a", "+x", " keep", "-b", "+y"]
        );
    }

    #[test]
    fn identical_texts_are_all_context() {
        let text = "one\ntwo\n";
        assert!(line_ops(text, text)
            .iter()
            .all(|op| matches!(op, LineOp::Equal(_))));
    }

    #[test]
    fn pure_insert_and_pure_remove() {
        assert_eq!(render(&line_ops("", "a\nb\n")), vec!["+a", "+b"]);
        assert_eq!(render(&line_ops("a\nb\n", "")), vec!["-a", "-b"]);
    }

    #[test]
    fn oversized_middle_falls_back_to_block_form() {
        // 1001×1001 несовпадающих строк — сверх потолка LCS.
        let old: String = (0..1001).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..1001).map(|i| format!("new {i}\n")).collect();
        let ops = line_ops(&old, &new);
        assert_eq!(ops.len(), 2002);
        assert!(ops[..1001].iter().all(|op| matches!(op, LineOp::Remove(_))));
        assert!(ops[1001..].iter().all(|op| matches!(op, LineOp::Insert(_))));
    }
}
