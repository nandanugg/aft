//! A small argument reader for `aft adapters …` and `aft facts …` (owner: R1).
//! Flags that take a value, flags that do not, and positional words. `--name=value` also works.

#[derive(Debug)]
pub(crate) struct Parsed {
    pub positional: Vec<String>,
    values: Vec<(String, String)>,
    switches: Vec<String>,
}

/// Reads `args`. `with_value` flags take the next word; `switches` take none.
/// Any other word that starts with `--` is an error.
pub(crate) fn parse(args: &[String], with_value: &[&str], switches: &[&str]) -> Result<Parsed, String> {
    let mut parsed = Parsed { positional: Vec::new(), values: Vec::new(), switches: Vec::new() };
    let mut index = 0;
    while index < args.len() {
        let word = &args[index];
        index += 1;
        let Some(flag) = word.strip_prefix("--") else {
            parsed.positional.push(word.clone());
            continue;
        };
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_string())),
            None => (flag, None),
        };
        if switches.contains(&name) {
            if inline.is_some() {
                return Err(format!("--{name} takes no value"));
            }
            parsed.switches.push(name.to_string());
        } else if with_value.contains(&name) {
            let value = match inline {
                Some(value) => value,
                None => {
                    let value = args.get(index).ok_or_else(|| format!("--{name} needs a value"))?;
                    index += 1;
                    value.clone()
                }
            };
            parsed.values.push((name.to_string(), value));
        } else {
            return Err(format!("unknown option --{name}"));
        }
    }
    Ok(parsed)
}

impl Parsed {
    /// The last value of `--name`.
    pub(crate) fn value(&self, name: &str) -> Option<&str> {
        self.values.iter().rev().find(|(flag, _)| flag == name).map(|(_, value)| value.as_str())
    }

    /// Every value of a repeatable `--name`.
    pub(crate) fn all(&self, name: &str) -> Vec<&str> {
        self.values.iter().filter(|(flag, _)| flag == name).map(|(_, value)| value.as_str()).collect()
    }

    pub(crate) fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|flag| flag == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn values_switches_and_positional_words_are_separated() {
        let parsed = parse(&words("implements a.go Doer --by=object --include-tag mock --include-tag init --json"), &["by", "include-tag"], &["json"]).unwrap();
        assert_eq!(parsed.positional, ["implements", "a.go", "Doer"]);
        assert_eq!(parsed.value("by"), Some("object"));
        assert_eq!(parsed.all("include-tag"), ["mock", "init"]);
        assert!(parsed.has("json") && !parsed.has("force"));
    }

    #[test]
    fn a_missing_value_and_an_unknown_option_are_errors() {
        assert!(parse(&words("--root"), &["root"], &[]).unwrap_err().contains("needs a value"));
        assert!(parse(&words("--rot x"), &["root"], &[]).unwrap_err().contains("unknown option --rot"));
    }
}
