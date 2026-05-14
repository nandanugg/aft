use regex::Regex;
use std::sync::LazyLock;

static CAMEL_CASE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z][A-Z]").unwrap());
static SNAKE_CASE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z]_[a-z]").unwrap());
static PASCAL_CASE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][a-z]+[A-Z]").unwrap());
static ACRONYM_PASCAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Z]{2,}[A-Z][a-z]").unwrap());
static DOT_PATH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-zA-Z]\.[a-zA-Z]").unwrap());
static FILE_PATH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[/\\].*\.\w{1,5}$").unwrap());
static HEX_CODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"0x[A-Fa-f0-9]+").unwrap());
static ERROR_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bERR_\w+").unwrap());
static NUMERIC_ERROR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bE\d{4,}").unwrap());
static HTTP_STATUS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[1-5]\d{2}\b").unwrap());

const QUESTION_WORDS: &[&str] = &[
    "how", "what", "where", "why", "when", "which", "who", "does",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    Identifier,
    Mixed,
    ErrorCode,
    Path,
    NaturalLanguage,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeWeights {
    pub semantic: f32,
    pub lexical: f32,
    pub should_use_lexical: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueryShape {
    pub kind: QueryKind,
    pub weights: ShapeWeights,
}

pub fn classify(query: &str) -> QueryShape {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return shape(QueryKind::NaturalLanguage);
    }

    let words: Vec<&str> = trimmed.split_whitespace().collect();
    let word_count = words.len();
    let first_word_lower = words[0].to_ascii_lowercase();

    if FILE_PATH_RE.is_match(trimmed) {
        return shape(QueryKind::Path);
    }

    let has_http_status = word_count <= 3 && HTTP_STATUS_RE.is_match(trimmed);
    if HEX_CODE_RE.is_match(trimmed)
        || ERROR_PREFIX_RE.is_match(trimmed)
        || NUMERIC_ERROR_RE.is_match(trimmed)
        || has_http_status
    {
        return shape(QueryKind::ErrorCode);
    }

    let has_code_identifier = CAMEL_CASE_RE.is_match(trimmed)
        || SNAKE_CASE_RE.is_match(trimmed)
        || PASCAL_CASE_RE.is_match(trimmed)
        || ACRONYM_PASCAL_RE.is_match(trimmed)
        || DOT_PATH_RE.is_match(trimmed);
    let has_question_word = QUESTION_WORDS.contains(&first_word_lower.as_str());
    let is_long_phrase = word_count > 2;
    let has_natural_language_signals = has_question_word || is_long_phrase;

    if has_code_identifier && has_natural_language_signals {
        return shape(QueryKind::Mixed);
    }

    if has_code_identifier || (word_count <= 2 && !has_natural_language_signals) {
        return shape(QueryKind::Identifier);
    }

    shape(QueryKind::NaturalLanguage)
}

fn shape(kind: QueryKind) -> QueryShape {
    QueryShape {
        kind,
        weights: weights_for(kind),
    }
}

fn weights_for(kind: QueryKind) -> ShapeWeights {
    match kind {
        QueryKind::Identifier => ShapeWeights {
            semantic: 0.2,
            lexical: 0.8,
            should_use_lexical: true,
        },
        QueryKind::Path | QueryKind::ErrorCode => ShapeWeights {
            semantic: 0.1,
            lexical: 0.9,
            should_use_lexical: true,
        },
        QueryKind::NaturalLanguage => ShapeWeights {
            semantic: 0.6,
            lexical: 0.4,
            should_use_lexical: false,
        },
        QueryKind::Mixed => ShapeWeights {
            semantic: 0.4,
            lexical: 0.6,
            should_use_lexical: true,
        },
    }
}
