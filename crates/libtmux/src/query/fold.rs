//! Unicode default case folding from `std` alone.
//!
//! `str::to_lowercase` is not a fold: it leaves `ß` alone, applies the final
//! sigma rule by context, and keeps `ς` distinct from `σ`. The lowercase of
//! the uppercase of the lowercase of each scalar value is: uppercasing
//! reaches the full mappings (`ß` to `SS`) that lowercasing does not, and the
//! last lowercase settles on one representative per class. One scalar value
//! is the exception, so it is handled by name.

/// Fold `text` so that two strings compare equal exactly when Unicode default
/// case folding, without normalization, makes them equal.
pub(super) fn default_case_fold_str(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for scalar in text.chars() {
        if scalar.is_ascii() {
            folded.push(scalar.to_ascii_lowercase());
        } else if scalar == '\u{131}' {
            // Dotless i uppercases to `I`, which would fold it onto `i`.
            // Default folding keeps it apart; only the Turkic mapping moves it.
            folded.push(scalar);
        } else {
            folded.extend(
                scalar
                    .to_lowercase()
                    .flat_map(char::to_uppercase)
                    .flat_map(char::to_lowercase),
            );
        }
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::default_case_fold_str;

    #[test]
    fn dotless_i_stays_apart_from_i() {
        assert_ne!(default_case_fold_str("\u{131}"), default_case_fold_str("i"));
        assert_ne!(default_case_fold_str("\u{131}"), default_case_fold_str("I"));
    }

    #[test]
    fn full_folding_reaches_what_lowercasing_does_not() {
        assert_eq!(default_case_fold_str("Stra\u{df}e"), "strasse");
        assert_eq!(
            default_case_fold_str("\u{3c2}"),
            default_case_fold_str("\u{3a3}")
        );
        assert_eq!(default_case_fold_str("\u{fb01}le"), "file");
    }
}
