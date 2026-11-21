//! The dashboard puts icon SVG text into the page with `inner_html` (ui/src/icons.rs), so an icon
//! must never carry anything that runs or loads: this fails on a script, an event handler, a link
//! or embedded content in any file of assets/icons and assets/logo.

#[test]
fn icons_hold_no_active_content() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    let bad = [
        "<script",
        "javascript:",
        "href",
        "<foreignobject",
        "data:",
        "<!doctype",
        "<!entity",
        "<iframe",
        "<embed",
        "<object",
        "@import",
    ];
    let mut checked = 0;
    for dir in ["icons", "logo"] {
        for entry in std::fs::read_dir(format!("{root}/{dir}")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "svg") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap().to_lowercase();
            for b in bad {
                assert!(!text.contains(b), "{} holds {b}", path.display());
            }
            // An attribute starting with "on" (onload=, onclick=) is an event handler.
            let handler = text
                .split(|c: char| c.is_whitespace())
                .any(|w| w.starts_with("on") && w.contains('='));
            assert!(
                !handler,
                "{} has an event handler attribute",
                path.display()
            );
            // Only local references (`url(#id)`), never a fetched file.
            for (i, _) in text.match_indices("url(") {
                assert!(
                    text[i + 4..].trim_start().starts_with('#'),
                    "{} has a url() that is not local",
                    path.display()
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 300, "only {checked} SVG files found");
}
