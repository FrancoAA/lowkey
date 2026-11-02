//! Text to Whisper tokens, in Rust: the vocabulary's prompt for whisper.cpp.
//!
//! whisper.cpp's own `whisper_tokenize` builds a C++ `std::regex`. In this app that aborts
//! ("free(): invalid pointer"): the static sherpa-onnx library (Parakeet) carries its own
//! instantiation of libstdc++'s regex code, built with another compiler, and the linker keeps
//! one copy for both. So the prompt is tokenized here, the way whisper.cpp does it: split the
//! text into words, then take the longest vocabulary entry at each position.

use std::collections::HashMap;

use regex::Regex;

/// Whisper's text tokens (bytes → id), without the special tokens.
pub struct WhisperVocab {
    ids: HashMap<Vec<u8>, i32>,
    longest: usize,
}

impl WhisperVocab {
    /// Every text token of the model: the ids below end-of-text.
    pub fn from_context(ctx: &whisper_rs::WhisperContext) -> Self {
        let eot = ctx.token_eot();
        Self::from_tokens((0..eot).filter_map(|id| ctx.token_to_bytes(id).ok().map(|b| (b.to_vec(), id))))
    }

    pub fn from_tokens(tokens: impl IntoIterator<Item = (Vec<u8>, i32)>) -> Self {
        let mut ids = HashMap::new();
        let mut longest = 1;
        for (bytes, id) in tokens {
            if !bytes.is_empty() {
                longest = longest.max(bytes.len());
                ids.entry(bytes).or_insert(id);
            }
        }
        Self { ids, longest }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The tokens of `text`; a byte no token covers is skipped (whisper.cpp does the same).
    pub fn tokenize(&self, text: &str, max_tokens: usize) -> Vec<i32> {
        let mut out = Vec::new();
        for word in words(text) {
            let bytes = word.as_bytes();
            let mut i = 0;
            while i < bytes.len() && out.len() < max_tokens {
                let mut j = bytes.len().min(i + self.longest);
                let mut found = false;
                while j > i {
                    if let Some(&id) = self.ids.get(&bytes[i..j]) {
                        out.push(id);
                        i = j;
                        found = true;
                        break;
                    }
                    j -= 1;
                }
                if !found {
                    i += 1;
                }
            }
        }
        out
    }
}

/// GPT-2's word split, as whisper.cpp writes it (without its look-ahead alternative, which
/// the regex crate lacks; `\s+` covers it).
fn words(text: &str) -> Vec<&str> {
    static SPLIT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = SPLIT.get_or_init(|| {
        Regex::new(r"'s|'t|'re|'ve|'m|'ll|'d| ?\p{Alphabetic}+| ?\p{Nd}+| ?[^\s\p{Alphabetic}\p{Nd}]+|\s+").expect("word split")
    });
    re.find_iter(text).map(|m| m.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab(entries: &[&str]) -> WhisperVocab {
        WhisperVocab::from_tokens(entries.iter().enumerate().map(|(i, s)| (s.as_bytes().to_vec(), i as i32)))
    }

    #[test]
    fn words_are_split_like_gpt2() {
        assert_eq!(words("Glossary: Lisandro, NMR-24354."), vec!["Glossary", ":", " Lisandro", ",", " NMR", "-", "24354", "."]);
        assert_eq!(words("it's Ñandú"), vec!["it", "'s", " Ñandú"]);
    }

    #[test]
    fn the_longest_token_wins_and_unknown_bytes_are_skipped() {
        let v = vocab(&["G", "loss", "ary", "Gloss", ":", " Lis", "andro", " L", "is", ",", " N", "MR", "."]);
        // "Glossary" -> "Gloss" + "ary" (longest first), " Lisandro" -> " Lis" + "andro".
        assert_eq!(v.tokenize("Glossary: Lisandro, NMR.", 100), vec![3, 2, 4, 5, 6, 9, 10, 11, 12]);
        assert_eq!(v.tokenize("Glossary: Lisandro", 2), vec![3, 2], "never more than asked");
        assert_eq!(v.tokenize("#", 10), Vec::<i32>::new(), "no token covers it: skipped");
        assert_eq!(v.len(), 13);
    }
}
