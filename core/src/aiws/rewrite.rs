//! Rewriting absolute paths inside AI tools' files when they move to a PC
//! where the user name, drive or project folders differ.

use std::collections::HashMap;

use regex::{Captures, Regex};

/// Swaps old path prefixes for new ones, longest first, in the spellings
/// these tools write: `C:\x`, JSON-escaped `C:\\x` (and doubly escaped), and
/// `C:/x`. Matching is case-insensitive and only on whole path segments, so
/// `C:\Users\Max` never rewrites `C:\Users\Maxine`.
pub struct Rewriter {
    re: Regex,
    map: HashMap<String, String>,
}

pub fn normalize(p: &str) -> String {
    p.trim().trim_end_matches(['\\', '/']).to_string()
}

impl Rewriter {
    pub fn new(pairs: &[(String, String)]) -> Option<Self> {
        let mut map: HashMap<String, String> = HashMap::new();
        for (from, to) in pairs {
            let (from, to) = (normalize(from), normalize(to));
            if from.is_empty() || to.is_empty() || from.eq_ignore_ascii_case(&to) {
                continue;
            }
            let variants = [
                (from.clone(), to.clone()),
                (from.replace('\\', "\\\\"), to.replace('\\', "\\\\")),
                (from.replace('\\', "\\\\\\\\"), to.replace('\\', "\\\\\\\\")),
                (from.replace('\\', "/"), to.replace('\\', "/")),
            ];
            for (f, t) in variants {
                map.entry(f.to_lowercase()).or_insert(t);
            }
        }
        if map.is_empty() {
            return None;
        }
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        let alts: Vec<String> = keys.iter().map(|k| regex::escape(k)).collect();
        // No look-ahead in this regex engine, so the boundary is captured
        // and put back.
        let pattern = format!(r#"(?i)(?:{})([\\/"'`\r\n\t<>|*?,;)\]]|$)"#, alts.join("|"));
        Some(Self { re: Regex::new(&pattern).ok()?, map })
    }

    pub fn apply(&self, text: &str) -> String {
        self.re
            .replace_all(text, |c: &Captures| {
                let whole = c.get(0).unwrap().as_str();
                let boundary = c.get(1).map_or("", |m| m.as_str());
                let key = &whole[..whole.len() - boundary.len()];
                format!("{}{}", self.map.get(&key.to_lowercase()).map_or(key, |s| s.as_str()), boundary)
            })
            .into_owned()
    }
}

/// Claude Code names each project's history folder after its working
/// directory, with everything but letters and digits turned into dashes.
pub fn project_folder_name(path: &str) -> String {
    normalize(path).chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Map a history folder name through path pairs when its working directory
/// is unknown.
pub fn map_folder_name(folder: &str, pairs: &[(String, String)]) -> String {
    let mut enc: Vec<(String, String)> = pairs.iter().map(|(f, t)| (project_folder_name(f), project_folder_name(t))).collect();
    enc.sort_by_key(|(f, _)| std::cmp::Reverse(f.len()));
    for (f, t) in enc {
        if folder.len() >= f.len()
            && folder[..f.len()].eq_ignore_ascii_case(&f)
            && (folder.len() == f.len() || folder.as_bytes()[f.len()] == b'-')
        {
            return format!("{t}{}", &folder[f.len()..]);
        }
    }
    folder.to_string()
}

pub fn is_under(path: &str, parent: &str) -> bool {
    let (p, q) = (normalize(path).to_lowercase(), normalize(parent).to_lowercase());
    p == q || p.starts_with(&format!("{q}\\")) || p.starts_with(&format!("{q}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rw() -> Rewriter {
        Rewriter::new(&[
            (r"C:\Users\Max".into(), r"C:\Users\Sam".into()),
            (r"C:\Users\Max\Desktop\Budget".into(), r"D:\Work\Budget".into()),
        ])
        .unwrap()
    }

    #[test]
    fn rewrites_all_spellings_longest_first() {
        let r = rw();
        assert_eq!(r.apply(r"cd C:\Users\Max\Desktop\Budget\src"), r"cd D:\Work\Budget\src");
        assert_eq!(r.apply(r#"{"cwd":"C:\\Users\\Max\\notes"}"#), r#"{"cwd":"C:\\Users\\Sam\\notes"}"#);
        assert_eq!(r.apply("c:/users/max/.claude/skills"), "C:/Users/Sam/.claude/skills");
        assert_eq!(r.apply(r"C:\Users\Max"), r"C:\Users\Sam");
    }

    #[test]
    fn leaves_longer_names_alone() {
        assert_eq!(rw().apply(r"C:\Users\Maxine\x"), r"C:\Users\Maxine\x");
    }

    #[test]
    fn folder_names() {
        assert_eq!(project_folder_name(r"C:\Users\X\Desktop\Clients\1. Main Workspace"), "C--Users-X-Desktop-Clients-1--Main-Workspace");
        assert_eq!(project_folder_name(r"D:\Claude Does Things"), "D--Claude-Does-Things");
        let pairs = vec![(r"C:\Users\Max".to_string(), r"C:\Users\Sam".to_string())];
        assert_eq!(map_folder_name("C--Users-Max-Desktop-App", &pairs), "C--Users-Sam-Desktop-App");
        assert_eq!(map_folder_name("C--Users-Maxine", &pairs), "C--Users-Maxine");
        assert!(is_under(r"D:\A\b", r"d:\a"));
        assert!(!is_under(r"D:\Ab", r"D:\A"));
    }
}
