//! Vocabulary: names and terms the speech model gets wrong (people, projects, products).
//!
//! Each term has optional "sounds like" spellings that a transcript shows instead. After every
//! transcription (dictation, files, meetings) those spellings, and the term in another case,
//! are replaced by the term, whole words only. Whisper also gets the terms as its initial
//! prompt, which makes it spell them right more often; Parakeet has no prompt, so only the
//! replacement helps there. The AI notes get the list too. (Idea from Hex, anomalyco/hex, MIT.)

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Term {
    pub term: String,
    /// Spellings the transcript shows instead ("Lisando", "Lee Sandro").
    pub sounds_like: Vec<String>,
}

pub const MAX_TERMS: usize = 200;
pub const MAX_CHARS: usize = 64;
pub const MAX_SPELLINGS: usize = 10;

/// The list as the app uses it: cleaned, with one matcher per term.
#[derive(Debug, Default)]
pub struct Vocabulary {
    terms: Vec<Term>,
    rules: Vec<(Regex, String)>,
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_CHARS).collect()
}

/// Blank terms and spellings out, lengths and counts cut to the limits, duplicates once.
pub fn normalize(terms: &[Term]) -> Vec<Term> {
    let mut out: Vec<Term> = Vec::new();
    for t in terms {
        let term = clean(&t.term);
        if term.is_empty() || out.iter().any(|o| o.term.eq_ignore_ascii_case(&term)) {
            continue;
        }
        let mut sounds: Vec<String> = Vec::new();
        for s in &t.sounds_like {
            let s = clean(s);
            if !s.is_empty() && !s.eq_ignore_ascii_case(&term) && !sounds.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
                sounds.push(s);
            }
        }
        sounds.truncate(MAX_SPELLINGS);
        out.push(Term { term, sounds_like: sounds });
        if out.len() == MAX_TERMS {
            break;
        }
    }
    out
}

/// A whole-word, case-insensitive pattern for one spelling. `\b` only at an edge that is a
/// word character ("C++" has none at its end).
fn pattern(spelling: &str) -> String {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let mut p = String::new();
    if word(spelling.chars().next()) {
        p.push_str(r"\b");
    }
    // Any run of spaces in the transcript matches one space here.
    p.push_str(&spelling.split(' ').map(regex::escape).collect::<Vec<_>>().join(r"\s+"));
    if word(spelling.chars().last()) {
        p.push_str(r"\b");
    }
    p
}

impl Vocabulary {
    pub fn new(terms: &[Term]) -> Self {
        let terms = normalize(terms);
        let mut rules = Vec::new();
        for t in &terms {
            // Longer spellings first, so "Lee Sandro" wins over "Sandro".
            let mut spellings: Vec<&str> = t.sounds_like.iter().map(String::as_str).chain([t.term.as_str()]).collect();
            spellings.sort_by_key(|s| std::cmp::Reverse(s.chars().count()));
            let alts: Vec<String> = spellings.iter().map(|s| pattern(s)).collect();
            if let Ok(re) = Regex::new(&format!("(?i)(?:{})", alts.join("|"))) {
                rules.push((re, t.term.clone()));
            }
        }
        Self { terms, rules }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn terms(&self) -> &[Term] {
        &self.terms
    }

    /// The text with every spelling of a term written as the term.
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (re, term) in &self.rules {
            if re.is_match(&out) {
                out = re.replace_all(&out, regex::NoExpand(term)).into_owned();
            }
        }
        out
    }

    /// Whisper's initial prompt: the terms as a short glossary (its prompt holds about 220
    /// tokens; a longer list is cut).
    pub fn whisper_prompt(&self) -> Option<String> {
        if self.terms.is_empty() {
            return None;
        }
        let mut s = String::from("Glossary:");
        for t in &self.terms {
            if s.len() + t.term.len() + 2 > 600 {
                break;
            }
            s.push(' ');
            s.push_str(&t.term);
            s.push(',');
        }
        s.pop();
        s.push('.');
        Some(s)
    }

    /// The terms, for the AI notes' instructions.
    pub fn names(&self) -> Vec<String> {
        self.terms.iter().map(|t| t.term.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(term: &str, sounds: &[&str]) -> Term {
        Term { term: term.into(), sounds_like: sounds.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn spellings_become_the_term_whole_words_only() {
        let v = Vocabulary::new(&[t("Lisandro", &["Lisando", "Lee Sandro"]), t("NMR", &["N M R", "enemar"]), t("Merf West", &["Murph west"])]);
        assert_eq!(v.apply("lisando and Lee  Sandro talked"), "Lisandro and Lisandro talked");
        assert_eq!(v.apply("ticket n m r-24354 for enemar"), "ticket NMR-24354 for NMR");
        assert_eq!(v.apply("check murph West today"), "check Merf West today");
        assert_eq!(v.apply("lisandro"), "Lisandro", "the term in another case is fixed too");
        assert_eq!(v.apply("Lisandrooo and Lisandos"), "Lisandrooo and Lisandos", "never inside a word");
        assert_eq!(Vocabulary::new(&[]).apply("anything at all"), "anything at all");
    }

    #[test]
    fn odd_characters_are_literal_and_unicode_words_work() {
        let v = Vocabulary::new(&[t("C++", &["c plus plus"]), t("Ñandú", &["nandu"]), t("$HOME", &["dollar home"])]);
        assert_eq!(v.apply("I like c plus plus and c++"), "I like C++ and C++");
        assert_eq!(v.apply("un nandu corre"), "un Ñandú corre");
        assert_eq!(v.apply("go to dollar home"), "go to $HOME", "$ in the term is not a regex group");
    }

    #[test]
    fn the_list_is_cleaned_and_limited() {
        let long = "x".repeat(100);
        let v = normalize(&[t("  Pablo  ", &["", "pablo", "Pavlo", "pavlo"]), t("pablo", &[]), t("", &["x"]), t(&long, &[])]);
        assert_eq!(v[0], t("Pablo", &["Pavlo"]), "blank, same-as-term and duplicate spellings go");
        assert_eq!(v.len(), 2, "a duplicate term and a blank term go");
        assert_eq!(v[1].term.chars().count(), MAX_CHARS);
        let many: Vec<Term> = (0..300).map(|i| t(&format!("term{i}"), &[])).collect();
        assert_eq!(normalize(&many).len(), MAX_TERMS);
        assert_eq!(Vocabulary::new(&[t("Lisandro", &[]), t("NMR", &[])]).whisper_prompt().as_deref(), Some("Glossary: Lisandro, NMR."));
        assert_eq!(Vocabulary::new(&[]).whisper_prompt(), None);
    }
}
