use super::*;

#[test]
fn compose_wrap_breaks_at_words_with_hanging_indent() {
    // width 14, prefix 4 → 10 columns per line on every row.
    let lines = break_into_visual_lines("check filter này ok", 14, 4);
    assert_eq!(lines, vec!["check ", "filter này ", "ok"]);
    assert_eq!(wrap_line_count("check filter này ok", 14, 4), 3);
    // Cursor after "fil" sits on line 1, col 3 (drawn at x = 4 + 3).
    assert_eq!(cursor_wrap_line("check filter", 9, 14, 4), 1);
    assert_eq!(cursor_wrap_col("check filter", 9, 14, 4), 3);
}

#[test]
fn compose_wrap_splits_words_longer_than_a_line() {
    let lines = break_into_visual_lines("abcdefghijklmn", 14, 4);
    assert_eq!(lines, vec!["abcdefghij", "klmn"]);
}
