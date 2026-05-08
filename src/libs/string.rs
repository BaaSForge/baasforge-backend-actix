pub fn truncate_by_words<S: AsRef<str>>(text: S, max_words: usize) -> Option<String> {
    let words: Vec<&str> = text.as_ref().split_whitespace().take(max_words).collect();

    if words.is_empty() {
        None
    } else {
        Some(words.join(" "))
    }
}
