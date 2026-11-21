//! Stored XSS checks for the Discord preview: saved text from a server goes through `md`,
//! `inline` and the button emoji into `inner_html`, so the HTML must hold no markup the text made.
use super::emoji;
use crate::md::{esc, inline, md};
use serde_json::json;

const SVG: &[&str] = &[
    "svg",
    "defs",
    "lineargradient",
    "stop",
    "mask",
    "g",
    "path",
    "rect",
    "circle",
    "ellipse",
];

/// One problem in the HTML; `exec` is true for one that can run script.
#[derive(Debug)]
struct Bad {
    exec: bool,
    what: String,
}

/// A small HTML tokenizer (data, tag and attribute states as in the HTML spec, no entities in
/// text) that lists each tag or attribute the renderer does not emit itself.
fn scan(h: &str) -> Vec<Bad> {
    let mut bad = Vec::new();
    let mut flag = |exec: bool, what: String| bad.push(Bad { exec, what });
    let b = h.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let end = b.get(i + 1) == Some(&b'/');
        let s = i + 1 + end as usize;
        match b.get(s) {
            Some(c) if c.is_ascii_alphabetic() => {}
            Some(b'!' | b'?') => {
                flag(false, format!("markup declaration at {i}"));
                i = s;
                continue;
            }
            _ => {
                i += 1;
                continue;
            }
        }
        let mut j = s;
        while j < b.len() && !b" \t\n\x0c\r/>".contains(&b[j]) {
            j += 1;
        }
        let tag = h[s..j].to_ascii_lowercase();
        let svg = SVG.contains(&tag.as_str());
        let ok_tags = [
            "div", "b", "i", "u", "s", "span", "code", "pre", "a", "img", "br",
        ];
        if !svg && !ok_tags.contains(&tag.as_str()) {
            flag(true, format!("tag <{tag}>"));
        }
        // attributes
        loop {
            while j < b.len() && b" \t\n\x0c\r/".contains(&b[j]) {
                j += 1;
            }
            if j >= b.len() || b[j] == b'>' {
                break;
            }
            let n0 = j;
            j += 1; // a first `=` belongs to the name
            while j < b.len() && !b" \t\n\x0c\r/>=".contains(&b[j]) {
                j += 1;
            }
            let name = h[n0..j].to_ascii_lowercase();
            while j < b.len() && b" \t\n\x0c\r".contains(&b[j]) {
                j += 1;
            }
            let mut val = String::new();
            if b.get(j) == Some(&b'=') {
                j += 1;
                while j < b.len() && b" \t\n\x0c\r".contains(&b[j]) {
                    j += 1;
                }
                match b.get(j) {
                    Some(&q @ (b'"' | b'\'')) => {
                        let v0 = j + 1;
                        j = v0;
                        while j < b.len() && b[j] != q {
                            j += 1;
                        }
                        val = h[v0..j.min(b.len())].to_string();
                        j += 1;
                    }
                    _ => {
                        let v0 = j;
                        while j < b.len() && !b" \t\n\x0c\r>".contains(&b[j]) {
                            j += 1;
                        }
                        val = h[v0..j].to_string();
                    }
                }
            }
            if end {
                continue;
            }
            if name.starts_with("on") {
                flag(true, format!("<{tag} {name}=\"{val}\">"));
            }
            if [
                "href",
                "src",
                "xlink:href",
                "action",
                "formaction",
                "srcdoc",
            ]
            .contains(&name.as_str())
            {
                let u = decode(&val);
                let scheme_ok = u.starts_with("https://") || u.starts_with("http://");
                if !scheme_ok
                    || ["javascript:", "data:", "vbscript:"]
                        .iter()
                        .any(|p| u.starts_with(p))
                {
                    flag(true, format!("<{tag} {name}=\"{val}\">"));
                }
            }
            let ok_attrs: &[&str] = match tag.as_str() {
                "div" => &["class"],
                "span" => &["class", "aria-hidden"],
                "a" => &["href", "target", "rel"],
                "img" => &["class", "alt", "src"],
                _ => &[],
            };
            if svg {
                if name == "style" || name.contains('"') || name.contains('\'') {
                    flag(false, format!("<{tag} {name}>"));
                }
            } else if !ok_attrs.contains(&name.as_str()) {
                flag(name == "style", format!("<{tag} {name}=\"{val}\">"));
            }
        }
        i = j + 1;
    }
    bad
}

/// Attribute value as the browser reads it: entities decoded, controls and spaces stripped, lower case.
fn decode(v: &str) -> String {
    let mut out = String::new();
    let mut r = v;
    while let Some(c) = r.chars().next() {
        if c == '&' {
            if let Some(e) = r.find(';').filter(|&e| e < 12) {
                let ent = &r[1..e];
                let ch = match ent {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "colon" => Some(':'),
                    "tab" | "newline" => Some(' '),
                    _ => ent
                        .strip_prefix("#x")
                        .or_else(|| ent.strip_prefix("#X"))
                        .and_then(|x| u32::from_str_radix(x, 16).ok())
                        .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                        .and_then(char::from_u32),
                };
                if let Some(ch) = ch {
                    out.push(ch);
                    r = &r[e + 1..];
                    continue;
                }
            }
        }
        out.push(c);
        r = &r[c.len_utf8()..];
    }
    out.chars()
        .filter(|c| !c.is_control() && !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Every renderer that feeds `inner_html`, with the escaping its caller applies.
fn renders(s: &str) -> Vec<(&'static str, String)> {
    vec![
        ("md", md(s)),
        ("inline(esc)", inline(&esc(s))),
        (
            "emoji name",
            emoji(&json!({ "name": s })).unwrap_or_default(),
        ),
        (
            "emoji name+id",
            emoji(&json!({ "name": s, "id": "123", "animated": true })).unwrap_or_default(),
        ),
        (
            "emoji id",
            emoji(&json!({ "name": "x", "id": s })).unwrap_or_default(),
        ),
    ]
}

const CORPUS: &[&str] = &[
    "<script>alert(1)</script>",
    "<img src=x onerror=alert(1)>",
    "<svg/onload=alert(1)>",
    "\"><script>alert(1)</script>",
    "'><img src=x onerror=alert(1)>",
    "[x](javascript:alert(1))",
    "[x](JaVaScRiPt:alert(1))",
    "[x](data:text/html,<script>alert(1)</script>)",
    "[x](vbscript:msgbox(1))",
    "[x](https://a\" onmouseover=\"alert(1))",
    "[x](https://a' onmouseover='alert(1))",
    "[x](https://a onmouseover=alert(1))",
    "[x](https://a&quot; onmouseover=alert(1))",
    "[\"><img src=x onerror=alert(1)>](https://a)",
    "[**x**](https://a/**b**\" onclick=\"1)",
    "[`x`](https://a/`b`)",
    "[x](https://a)[y](https://b\" x=\")",
    "[[x](https://a)](https://b)",
    "[x](https://a/{e:case}\" onclick=\"alert(1))",
    "[x](https://a{e:case}onmouseover=alert(1)//)",
    "<:{e:case}:1> x\" onerror=alert(1) y",
    "<:a\" onerror=\"alert(1):123>",
    "<a:x onerror=alert(1):1>",
    "<:x:1 onerror=alert(1)>",
    "<:\u{1}0\u{1}:1> [y](https://a\" onerror=alert(1) b)",
    "\u{1}0\u{1}[x](https://a)",
    "<@1\" onclick=\"x> <@!2> <@&3> <#4> <#x\">",
    "<t:1:\"onmouseover=alert(1)> <t:+1:R> <t:x:R> <t:99999999999999999999:f>",
    "<t:1:<script>>",
    "`<script>alert(1)</script>`",
    "```html\n<script>alert(1)</script>\n```",
    "```\n\"><svg onload=alert(1)>```",
    "```unclosed <b onclick=x>",
    "||<img src=x onerror=1>||",
    "# <script>\n## \"x\"\n### '\n-# <i>\n> <q>\n- <li>\n* <li>",
    "**__~~||*_[x](https://a\"b)_*||~~__**",
    "&lt;script&gt;alert(1)&lt;/script&gt;",
    "&#60;script&#62; &#x3C;img src=x onerror=1&#x3E;",
    "&quot;&gt;&lt;svg onload=1&gt;",
    "\u{FF1C}script\u{FF1E}alert(1)\u{FF1C}/script\u{FF1E}",
    "<\u{200B}script>alert(1)</script>",
    "\u{200B}[x](\u{200B}javascript:alert(1))",
    "%3Cscript%3Ealert(1)%3C/script%3E",
    "[x](https://a/%22%3E%3Cscript%3E)",
    "<!-- x --><![CDATA[x]]><?x?>",
    "{e:case}{e:nope}{e:{e:case}}{e:",
    "<:{e:case}:1>",
    "[{e:case}](https://a)",
    "`{e:case}` <@{e:case}>",
    "<style>*{x:y}</style><iframe src=javascript:1>",
];

#[test]
fn no_input_makes_script_or_markup() {
    let long = format!(
        "{}{}{}",
        "[x](https://a\"".repeat(2000),
        "<script>".repeat(2000),
        "**_||`".repeat(5000)
    );
    let (mut cases, mut exec, mut other) = (0, Vec::new(), Vec::new());
    for s in CORPUS.iter().copied().chain([long.as_str()]) {
        for (who, h) in renders(s) {
            cases += 1;
            for b in scan(&h) {
                let line = format!("{who}: {s:?} -> {} in {h}", b.what);
                if b.exec {
                    exec.push(line)
                } else {
                    other.push(line)
                }
            }
        }
    }
    eprintln!(
        "corpus: {cases} renders, {} exec, {} other",
        exec.len(),
        other.len()
    );
    for l in &other {
        eprintln!("other: {}", &l[..l.len().min(600)]);
    }
    assert!(exec.is_empty(), "{exec:#?}");
}

/// Random mixes of markdown and HTML pieces through every renderer.
#[test]
fn fuzz_makes_no_script() {
    const T: &[&str] = &[
        "<",
        ">",
        "\"",
        "'",
        "&",
        ";",
        "=",
        " ",
        "\n",
        "`",
        "```",
        "[",
        "]",
        "(",
        ")",
        "](",
        "https://",
        "http://",
        "javascript:",
        "data:",
        "*",
        "**",
        "_",
        "__",
        "~~",
        "||",
        "# ",
        "> ",
        "- ",
        "-# ",
        "<:",
        "<a:",
        ":",
        "123",
        "<@",
        "<@&",
        "<#",
        "<t:",
        ":R",
        "{e:",
        "case",
        "}",
        "\u{1}",
        "0",
        "\u{1}0\u{1}",
        "onerror=",
        "onclick",
        "alert(1)",
        "script",
        "img",
        "svg",
        "src",
        "href",
        "&lt;",
        "&gt;",
        "&quot;",
        "&amp;",
        "&#",
        "x3C",
        "\u{FF1C}",
        "\u{200B}",
        "%3C",
        "/",
        "!--",
        "a",
        "é",
    ];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let (n, mut runs, mut other, mut first_other) = (200_000, 0, 0, None);
    for _ in 0..n {
        let len = 1 + next() % 24;
        let s: String = (0..len)
            .map(|_| T[(next() % T.len() as u64) as usize])
            .collect();
        for (who, h) in renders(&s) {
            runs += 1;
            let bad = scan(&h);
            if let Some(b) = bad.iter().find(|b| b.exec) {
                panic!("{who}: {s:?} -> {} in {h}", b.what);
            }
            if !bad.is_empty() {
                other += 1;
                first_other.get_or_insert(format!("{who}: {s:?} -> {h}"));
            }
        }
    }
    eprintln!(
        "fuzz: {n} inputs, {runs} renders, 0 exec, {other} other; first other: {first_other:?}"
    );
}

/// `md` fills `{e:name}` tags after the HTML is built: a tag inside a link target must stay
/// encoded, or the icon's quotes close `href` early and break the link.
#[test]
fn an_icon_tag_stays_out_of_attributes() {
    let h = md("[x](https://a/{e:case})");
    let bad = scan(&h);
    assert!(bad.is_empty(), "{bad:?} in {h}");
}
