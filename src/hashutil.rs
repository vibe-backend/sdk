use sha2::{Digest, Sha256};

/// Feed one UTF-8 field using the compile prompt fingerprint encoding.
pub fn hash_length_prefixed(hash: &mut Sha256, value: &str) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value.as_bytes());
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

pub fn sha256_prefixed(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

/// Normalize source text before hashing.
///
/// Unifies newlines, strips a leading BOM, trims trailing whitespace on each
/// line, and collapses runs of blank lines to a single blank line.
pub fn normalize_source(input: &str) -> String {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let unified = input.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = unified.split('\n').map(str::trim_end).collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let mut out = String::new();
    let mut previous_blank = false;
    for line in lines {
        if line.is_empty() {
            if !previous_blank {
                out.push('\n');
                previous_blank = true;
            }
        } else {
            previous_blank = false;
            out.push_str(line);
            out.push('\n');
        }
    }
    if out.is_empty() {
        out.push('\n');
    }
    out
}

pub fn source_hash(input: &str) -> String {
    sha256_prefixed(normalize_source(input).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_empty_known_answer() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn normalization_ignores_layout_only_edits() {
        let a = "hello  \r\n\r\n\r\nworld\n";
        let b = "\u{feff}hello\n\nworld\n";
        assert_eq!(normalize_source(a), "hello\n\nworld\n");
        assert_eq!(normalize_source(a), normalize_source(b));
        assert_eq!(source_hash(a), source_hash(b));
    }
}
