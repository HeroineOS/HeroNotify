//! Text as apps send it.

/// Markup some apps send anyway (`<b>`, `<a href>`, `&amp;`) made plain
/// text; whitespace runs collapsed, lines kept.
pub fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                // A tag: dropped (a <br> is a new line).
                let mut tag = String::new();
                for t in chars.by_ref() {
                    if t == '>' {
                        break;
                    }
                    tag.push(t);
                }
                let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
                if name == "br" || name == "p" && tag.starts_with('/') {
                    out.push('\n');
                }
            }
            '&' => {
                let mut ent = String::new();
                while let Some(&t) = chars.peek() {
                    if t == ';' || ent.len() > 8 || t.is_whitespace() {
                        break;
                    }
                    ent.push(t);
                    chars.next();
                }
                let decoded = match ent.as_str() {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
                    e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
                    _ => None,
                };
                match decoded {
                    Some(d) if chars.peek() == Some(&';') => {
                        chars.next();
                        out.push(d);
                    }
                    _ => {
                        out.push('&');
                        out.push_str(&ent);
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// A percent-encoded URI path decoded.
pub fn unescape_uri(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("x"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// "now", "5 min", "2 h", "3 days" ago.
pub fn ago(then: u64, now: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{} min", s / 60),
        3600..=86399 => format!("{} h", s / 3600),
        _ if s / 86400 == 1 => "yesterday".into(),
        _ => format!("{} days", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup() {
        assert_eq!(plain("<b>Bold</b> &amp; <i>more</i>"), "Bold & more");
        assert_eq!(plain("a<br/>b"), "a\nb");
        assert_eq!(plain("x &lt;3 &#233;t&#xE9;"), "x <3 été");
        assert_eq!(plain("Tom & Jerry"), "Tom & Jerry");
        assert_eq!(plain("<a href=\"https://x\">link</a>   text"), "link text");
        assert_eq!(plain("  two\n\n  lines "), "two\n\nlines");
    }

    #[test]
    fn uris() {
        assert_eq!(unescape_uri("/home/a%20b/%C3%BC.png"), "/home/a b/ü.png");
    }

    #[test]
    fn times() {
        assert_eq!(ago(100, 130), "now");
        assert_eq!(ago(0, 600), "10 min");
        assert_eq!(ago(0, 7200), "2 h");
        assert_eq!(ago(0, 90000), "yesterday");
        assert_eq!(ago(0, 3 * 86400), "3 days");
    }
}
