use serde_json::Value;

/// Source history search tokenizes across names, with within-word subsequences and bounded typos.
pub(super) fn matches(query: &str, agent: &Value, project: &Value) -> bool {
    let query = query.trim_matches(js_space).to_lowercase();
    if query.is_empty() {
        return true;
    }
    let fields = [
        &project["workspaceName"],
        &agent["title"],
        &project["checkout"]["currentBranch"],
        &project["projectName"],
    ]
    .map(|v| v.as_str().unwrap_or("").to_lowercase());
    let mut scratch = Scratch::default();
    query
        .split(js_space)
        .filter(|token| !token.is_empty())
        .all(|token| {
            fields
                .iter()
                .any(|field| token_matches(token, field, &mut scratch))
        })
}
fn token_matches(token: &str, field: &str, scratch: &mut Scratch) -> bool {
    if field.contains(token) {
        return true;
    }
    for word in field.split(js_space) {
        let mut chars = token.chars();
        let mut next = chars.next();
        for ch in word.chars() {
            if next == Some(ch) {
                next = chars.next();
            }
        }
        if next.is_none() {
            return true;
        }
    }
    scratch.query.clear();
    scratch.query.extend(token.encode_utf16());
    let query_length = scratch.query.len();
    if query_length <= 3 {
        return false;
    }
    let budget = if query_length <= 7 { 1 } else { 2 };
    for word in field
        .split(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit())
        .filter(|w| !w.is_empty())
    {
        let lengths = [
            word.len(),
            word.len().min(query_length),
            word.len().min(query_length + budget),
        ];
        for (position, length) in lengths.into_iter().enumerate() {
            if lengths[..position].contains(&length) {
                continue;
            }
            let word = &word.as_bytes()[..length];
            if query_length == 4 {
                let query = &scratch.query;
                if word.len() != query_length {
                    continue;
                }
                let Some(index) = query.iter().zip(word).position(|(a, b)| *a != *b as u16) else {
                    return true;
                };
                if index + 1 < word.len()
                    && query[index] == word[index + 1] as u16
                    && query[index + 1] == word[index] as u16
                    && query[index + 2..]
                        .iter()
                        .zip(&word[index + 2..])
                        .all(|(a, b)| *a == *b as u16)
                {
                    return true;
                }
            } else if scratch.distance(word, budget) {
                return true;
            }
        }
    }
    false
}
#[derive(Default)]
struct Scratch {
    query: Vec<u16>,
    previous: Vec<usize>,
    current: Vec<usize>,
    older: Vec<usize>,
}
impl Scratch {
    fn distance(&mut self, word: &[u8], budget: usize) -> bool {
        let query = &self.query;
        if query.len().abs_diff(word.len()) > budget {
            return false;
        }
        self.previous.clear();
        self.previous.extend(0..=word.len());
        self.current.resize(word.len() + 1, 0);
        self.older.resize(word.len() + 1, 0);
        for i in 1..=query.len() {
            self.current[0] = i;
            let mut best = i;
            for j in 1..=word.len() {
                let substitution = usize::from(query[i - 1] != word[j - 1] as u16);
                let mut cost = (self.current[j - 1] + 1)
                    .min(self.previous[j] + 1)
                    .min(self.previous[j - 1] + substitution);
                if i > 1
                    && j > 1
                    && query[i - 1] == word[j - 2] as u16
                    && query[i - 2] == word[j - 1] as u16
                {
                    cost = cost.min(self.older[j - 2] + 1);
                }
                self.current[j] = cost;
                best = best.min(cost);
            }
            if best > budget {
                return false;
            }
            std::mem::swap(&mut self.older, &mut self.previous);
            std::mem::swap(&mut self.previous, &mut self.current);
        }
        self.previous[word.len()] <= budget
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn history_matches_names_and_bounded_typos_not_transcripts() {
        let agent = json!({"title":"Configure permissions","transcript":"private unique content"});
        let project = json!({"projectName":"Paseo","workspaceName":"design"});
        assert!(matches("confug paseo", &agent, &project));
        assert!(!matches("unique", &agent, &project));
        assert!(matches("desgin", &agent, &project));
        assert!(!matches("despase", &agent, &project));
    }
    #[test]
    fn four_letter_tokens_only_allow_transpositions() {
        let mut scratch = Scratch::default();
        assert!(token_matches("mian", "main", &mut scratch));
        assert!(!token_matches("mail", "main", &mut scratch));
        assert!(!token_matches(
            "labdes",
            "Label as Design".to_lowercase().as_str(),
            &mut scratch
        ));
    }
}

fn js_space(character: char) -> bool {
    matches!(character,'\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
