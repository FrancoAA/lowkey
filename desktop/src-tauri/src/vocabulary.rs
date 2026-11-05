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

/// A fix in a transcript that looks like a name or a term the model gets wrong: the words that
/// changed (one to three on each side), as a term and how the model wrote it. None for other
/// fixes (a new sentence, a word removed, punctuation, a plain word such as "their" → "there":
/// the new words need a capital letter or a digit), and for a spelling the list already has.
pub fn suggestion(before: &str, after: &str, known: &[Term]) -> Option<Term> {
    let b: Vec<&str> = before.split_whitespace().collect();
    let a: Vec<&str> = after.split_whitespace().collect();
    let pre = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
    let suf = b[pre..].iter().rev().zip(a[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (old, new) = (&b[pre..b.len() - suf], &a[pre..a.len() - suf]);
    if !(1..=3).contains(&old.len()) || !(1..=3).contains(&new.len()) {
        return None;
    }
    // Sentence punctuation and quotes come off; the signs of a term (".NET", "C#", "$HOME") stay.
    let strip = |w: &[&str]| {
        w.join(" ")
            .trim_start_matches(|c: char| "\"'“‘«([{¿¡".contains(c))
            .trim_end_matches(|c: char| ".,;:!?\"'”’»)]}…".contains(c))
            .to_string()
    };
    let (old, new) = (strip(old), strip(new));
    if old.is_empty() || new.is_empty() || old.chars().count() > MAX_CHARS || new.chars().count() > MAX_CHARS {
        return None;
    }
    // Only the case changed ("we" → "We" at a sentence start): never a term (it would be
    // replaced in every transcript).
    if key(&old) == key(&new) {
        return None;
    }
    // A name or a term: a capital, a digit, or a script without case (Chinese, Arabic…).
    if !new.chars().any(|c| c.is_alphanumeric() && !c.is_lowercase()) {
        return None;
    }
    if has(known, &Term { term: new.clone(), sounds_like: vec![old.clone()] }) {
        return None;
    }
    Some(Term { term: new, sounds_like: vec![old] })
}

/// `t`'s term (any case) is in the list with each of its spellings.
pub fn has(terms: &[Term], t: &Term) -> bool {
    terms.iter().any(|x| key(&x.term) == key(&t.term) && t.sounds_like.iter().all(|s| key(s) == key(&x.term) || x.sounds_like.iter().any(|y| key(y) == key(s))))
}

/// The list with `t` in it: its spelling added to the term when the term is there already
/// (in any case). The list's limits apply: `has` tells whether it fit.
pub fn with_term(terms: &[Term], t: Term) -> Vec<Term> {
    let mut out = terms.to_vec();
    match out.iter_mut().find(|x| key(&x.term) == key(&t.term)) {
        Some(x) => {
            for s in t.sounds_like {
                if !x.sounds_like.iter().any(|y| key(y) == key(&s)) {
                    x.sounds_like.push(s);
                }
            }
        }
        None => out.push(t),
    }
    normalize(&out)
}

/// The list as the app uses it: cleaned, with one matcher per term.
#[derive(Debug, Default)]
pub struct Vocabulary {
    terms: Vec<Term>,
    /// Every spelling of every term in one pattern, longest first: one pass over the text, so
    /// a term's replacement is never rewritten by another term.
    pattern: Option<Regex>,
    /// A spelling (lower case, single spaces) -> its term.
    by_spelling: std::collections::HashMap<String, String>,
}

/// How a spelling is looked up: lower case, runs of spaces as one.
fn key(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
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
        let mut by_spelling = std::collections::HashMap::new();
        let mut spellings: Vec<&str> = Vec::new();
        for t in &terms {
            for s in t.sounds_like.iter().map(String::as_str).chain([t.term.as_str()]) {
                // The first term that claims a spelling keeps it.
                if !by_spelling.contains_key(&key(s)) {
                    by_spelling.insert(key(s), t.term.clone());
                    spellings.push(s);
                }
            }
        }
        // Longer spellings first: at one position the regex takes the first alternative that
        // matches, so "N M R" wins over "M R" and "Lee Sandro" over "Sandro".
        spellings.sort_by_key(|s| std::cmp::Reverse(s.chars().count()));
        let alts: Vec<String> = spellings.iter().map(|s| pattern(s)).collect();
        let pattern = (!alts.is_empty()).then(|| Regex::new(&format!("(?i)(?:{})", alts.join("|"))).ok()).flatten();
        Self { terms, pattern, by_spelling }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn terms(&self) -> &[Term] {
        &self.terms
    }

    /// The text with every spelling of a term written as the term.
    pub fn apply(&self, text: &str) -> String {
        let Some(re) = &self.pattern else { return text.to_string() };
        re.replace_all(text, |c: &regex::Captures| {
            let found = &c[0];
            self.by_spelling.get(&key(found)).cloned().unwrap_or_else(|| found.to_string())
        })
        .into_owned()
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

    #[test]
    fn a_fixed_name_is_offered_for_the_vocabulary() {
        let t = |term: &str, s: &str| Term { term: term.into(), sounds_like: vec![s.into()] };
        assert_eq!(suggestion("I talked to Lisando today.", "I talked to Lisandro today.", &[]), Some(t("Lisandro", "Lisando")));
        assert_eq!(suggestion("hello from open whisper.", "hello from OpenWhisper.", &[]), Some(t("OpenWhisper", "open whisper")));
        assert_eq!(suggestion("we use cube ernetes", "we use Kubernetes", &[]), Some(t("Kubernetes", "cube ernetes")));
        assert_eq!(suggestion("put it their", "put it there", &[]), None, "a plain word");
        assert_eq!(suggestion("a b c", "a b c d e f g h", &[]), None, "new words, not a fix");
        assert_eq!(suggestion("ask Lisando now", "ask now", &[]), None, "a word removed");
        assert_eq!(suggestion("ok, Lisandro", "ok. Lisandro", &[]), None, "punctuation only");
        assert_eq!(suggestion("to Lisando.", "to Lisandro.", &[t("Lisandro", "lisando")]), None, "already in the list");
        let list = with_term(&[t("Lisandro", "Lee Sandro")], t("Lisandro", "Lisando"));
        assert_eq!(list, vec![Term { term: "Lisandro".into(), sounds_like: vec!["Lee Sandro".into(), "Lisando".into()] }]);
        // Only the case: never a term.
        assert_eq!(suggestion("we use cube", "We use cube", &[]), None);
        assert_eq!(suggestion("i think so", "I think so", &[]), None);
        // The signs of a term stay; sentence punctuation goes.
        assert_eq!(suggestion("we use dot net.", "we use .NET.", &[]), Some(t(".NET", "dot net")));
        assert_eq!(suggestion("in c sharp", "in C#", &[]), Some(t("C#", "c sharp")));
        assert_eq!(suggestion("echo dollar home", "echo $HOME", &[]), Some(t("$HOME", "dollar home")));
        // A script without case.
        // No spaces between the words: the whole phrase is offered (the user can say No).
        assert_eq!(suggestion("我和李明说", "我和黎明说", &[]), Some(t("我和黎明说", "我和李明说")));
        assert_eq!(suggestion("with 李明 today", "with 黎明 today", &[]), Some(t("黎明", "李明")));
        // The term in another case in the list: one more spelling for it.
        let lower = with_term(&[t("lisandro", "Lee Sandro")], t("Lisandro", "Lisando"));
        assert_eq!(lower.len(), 1);
        assert!(has(&lower, &t("Lisandro", "Lisando")));
        assert_eq!(suggestion("to Lisando.", "to Lisandro.", &lower), None, "already there, in any case");
        // Full: has() says so.
        let full: Vec<Term> = (0..MAX_TERMS).map(|i| t(&format!("Term{i}"), &format!("tirm{i}"))).collect();
        assert!(!has(&with_term(&full, t("Lisandro", "Lisando")), &t("Lisandro", "Lisando")));
    }

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
    fn one_pass_the_longest_spelling_wins_and_a_term_is_never_rewritten() {
        // "M R" is listed first, but "N M R" is longer: it wins where both start.
        let v = Vocabulary::new(&[t("MR", &["M R"]), t("NMR", &["N M R"])]);
        assert_eq!(v.apply("ticket N M R-1 and M R"), "ticket NMR-1 and MR");
        // A term that is another term's spelling is not rewritten again.
        let v = Vocabulary::new(&[t("Open", &["opn"]), t("OpenWhisper", &["open whisper"])]);
        assert_eq!(v.apply("opn the open whisper app"), "Open the OpenWhisper app");
        assert_eq!(v.apply(&v.apply("opn the open whisper app")), "Open the OpenWhisper app", "applying twice changes nothing");
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
