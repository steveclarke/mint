//! The EFF large wordlist (7,776 words, CC BY 3.0 US, Electronic Frontier
//! Foundation), embedded at build time.

use std::sync::OnceLock;

const RAW: &str = include_str!("../data/eff_large_wordlist.txt");

pub fn words() -> &'static [&'static str] {
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| RAW.lines().map(str::trim).filter(|w| !w.is_empty()).collect())
}
