//! Подсветка синтаксиса блоков кода для приложения (ratatui).
//!
//! Использует syntect (те же ассеты, что REPL-рендер: `SyntaxSet` +
//! тема `base16-ocean.dark`), но выдаёт готовые ratatui `Line`/`Span` вместо
//! ANSI-escape строк. Ассеты грузятся один раз (`OnceLock`); подсветка идёт
//! построчно, результат кэшируется вызывающим.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

struct Assets {
    syntaxes: SyntaxSet,
    theme: Theme,
}

fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| Assets {
        syntaxes: SyntaxSet::load_defaults_newlines(),
        theme: ThemeSet::load_defaults()
            .themes
            .remove("base16-ocean.dark")
            .unwrap_or_default(),
    })
}

/// Подсвечивает блок кода языка `lang` в ratatui-строки фиксированной ширины
/// `width` с фоном `bg` (моно-панель на всю ширину). Неизвестный/пустой язык →
/// обычный текст без подсветки.
#[must_use]
pub fn highlight_block(code: &str, lang: &str, bg: Color, width: usize) -> Vec<Line<'static>> {
    let assets = assets();
    let syntax = find_syntax(assets, lang);
    let mut highlighter = HighlightLines::new(syntax, &assets.theme);
    let mut out = Vec::new();
    for line in LinesWithEndings::from(code) {
        let ranges = highlighter
            .highlight_line(line, &assets.syntaxes)
            .unwrap_or_default();
        out.push(styled_line(&ranges, bg, width));
    }
    if out.is_empty() {
        out.push(Line::from(Span::styled(
            " ".repeat(width),
            Style::default().bg(bg),
        )));
    }
    out
}

/// Подсвечивает diff (строки с префиксами `+`/`-`/пробел) синтаксисом языка
/// `lang`, **сохраняя цвета кода**. Add/remove показываются мягким фоном строки
/// и маркером в жёлобе, а не сплошной красно-зелёной заливкой (стиль git-delta).
/// Строки без префикса (напр. сводка «… ещё N строк») идут приглушённым текстом.
#[must_use]
pub fn highlight_diff(diff: &str, lang: &str, width: usize) -> Vec<Line<'static>> {
    let assets = assets();
    let syntax = find_syntax(assets, lang);
    let mut highlighter = HighlightLines::new(syntax, &assets.theme);

    let bg_ctx = Color::Rgb(30, 33, 44);
    let bg_add = Color::Rgb(26, 44, 33);
    let bg_del = Color::Rgb(50, 30, 34);
    let fg_add = Color::Rgb(126, 199, 130);
    let fg_del = Color::Rgb(224, 130, 140);
    let fg_muted = Color::Rgb(127, 132, 156);

    let code_w = width.saturating_sub(2).max(1); // 2 колонки под жёлоб «± »
    let mut out = Vec::new();

    for raw in diff.lines() {
        let (marker, bg, gutter_fg, content, do_highlight) = match raw.chars().next() {
            Some('+') => ('+', bg_add, fg_add, &raw[1..], true),
            Some('-') => ('-', bg_del, fg_del, &raw[1..], true),
            Some(' ') => (' ', bg_ctx, fg_muted, &raw[1..], true),
            _ => (' ', bg_ctx, fg_muted, raw, false),
        };

        let mut spans = vec![Span::styled(
            format!("{marker} "),
            Style::default()
                .fg(gutter_fg)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        )];

        if do_highlight {
            let ranges = highlighter
                .highlight_line(content, &assets.syntaxes)
                .unwrap_or_default();
            spans.extend(styled_spans(&ranges, bg, code_w));
        } else {
            let text: String = content.chars().take(code_w).collect();
            let used = text.chars().count();
            spans.push(Span::styled(text, Style::default().fg(fg_muted).bg(bg)));
            if used < code_w {
                spans.push(Span::styled(" ".repeat(code_w - used), Style::default().bg(bg)));
            }
        }
        out.push(Line::from(spans));
    }

    if out.is_empty() {
        out.push(Line::from(Span::styled(
            " ".repeat(width),
            Style::default().bg(bg_ctx),
        )));
    }
    out
}

fn find_syntax<'a>(assets: &'a Assets, lang: &str) -> &'a SyntaxReference {
    let lang = lang.trim();
    if !lang.is_empty() {
        if let Some(syntax) = assets
            .syntaxes
            .find_syntax_by_token(lang)
            .or_else(|| assets.syntaxes.find_syntax_by_extension(lang))
        {
            return syntax;
        }
    }
    assets.syntaxes.find_syntax_plain_text()
}

/// Строит одну ratatui-строку из подсвеченных диапазонов syntect, обрезая по
/// ширине и добивая фоном до правого края (ровная панель).
fn styled_line(
    ranges: &[(syntect::highlighting::Style, &str)],
    bg: Color,
    width: usize,
) -> Line<'static> {
    Line::from(styled_spans(ranges, bg, width))
}

/// Строит подсвеченные спаны из диапазонов syntect, обрезая по `width` и добивая
/// фоном до правого края. Общее для блоков кода и строк diff.
fn styled_spans(
    ranges: &[(syntect::highlighting::Style, &str)],
    bg: Color,
    width: usize,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for (style, text) in ranges {
        if used >= width {
            break;
        }
        let text = text.trim_end_matches(['\n', '\r']);
        if text.is_empty() {
            continue;
        }
        let slice: String = text.chars().take(width - used).collect();
        let count = slice.chars().count();
        if count == 0 {
            continue;
        }
        used += count;
        let fg = Color::Rgb(
            style.foreground.r,
            style.foreground.g,
            style.foreground.b,
        );
        spans.push(Span::styled(slice, Style::default().fg(fg).bg(bg)));
    }
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(bg),
        ));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::highlight_block;
    use ratatui::style::Color;

    #[test]
    fn highlights_rust_into_multiple_spans() {
        let code = "fn main() {\n    let x = 1;\n}";
        let lines = highlight_block(code, "rust", Color::Rgb(0, 0, 0), 40);
        assert_eq!(lines.len(), 3, "one ratatui line per code line");
        // Подсветка даёт несколько цветовых сегментов (ключевое слово/идентификатор/…).
        assert!(
            lines[0].spans.len() >= 2,
            "syntax should split the line into colored spans"
        );
    }

    #[test]
    fn diff_keeps_syntax_colors_and_marks_add_remove() {
        let diff = "-let x = 1;\n+let x = 2;\n";
        let lines = super::highlight_diff(diff, "rust", 40);
        assert_eq!(lines.len(), 2, "one ratatui line per diff line");
        // Жёлоб несёт маркер +/- (а не сплошную заливку всей строки).
        assert!(lines[0].spans[0].content.starts_with('-'), "remove marker");
        assert!(lines[1].spans[0].content.starts_with('+'), "add marker");
        // Подсветка кода сохраняется: строка бьётся на несколько цветовых спанов.
        assert!(
            lines[0].spans.len() >= 3,
            "syntax highlighting preserved inside the diff line"
        );
    }

    #[test]
    fn unknown_language_falls_back_without_panic() {
        let lines = highlight_block("plain text here", "no-such-lang", Color::Rgb(0, 0, 0), 20);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn each_line_padded_to_width() {
        let lines = highlight_block("x", "rust", Color::Rgb(0, 0, 0), 12);
        let total: usize = lines[0]
            .spans
            .iter()
            .map(|span| span.content.chars().count())
            .sum();
        assert!(total >= 12, "line padded to panel width");
    }
}
