//! Подсветка синтаксиса блоков кода для приложения (ratatui).
//!
//! Использует syntect (те же ассеты, что REPL-рендер: `SyntaxSet` +
//! тема `base16-ocean.dark`), но выдаёт готовые ratatui `Line`/`Span` вместо
//! ANSI-escape строк. Ассеты грузятся один раз (`OnceLock`); подсветка идёт
//! построчно, результат кэшируется вызывающим.

use std::sync::OnceLock;

use ratatui::style::{Color, Style};
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
    let cap = width.saturating_sub(1);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for (style, text) in ranges {
        if used >= cap {
            break;
        }
        let text = text.trim_end_matches(['\n', '\r']);
        if text.is_empty() {
            continue;
        }
        let slice: String = text.chars().take(cap - used).collect();
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
    Line::from(spans)
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
