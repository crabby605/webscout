use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::error::{Result, ScoutError};

/// Tags whose text is never shown to a reader.
const HIDDEN: &[&str] = &["script", "style", "noscript", "template", "head"];

/// Parsed HTML document (html5ever, spec-compliant error recovery).
pub struct Dom {
    doc: Html,
    base: Option<Url>,
}

impl Dom {
    pub fn parse(html: &str, base_url: &str) -> Self {
        let doc = Html::parse_document(html);
        let page = Url::parse(base_url).ok();
        // <base href> overrides the page URL for resolving relative links.
        let base = match (&page, sel("base[href]").ok().and_then(|s| doc.select(&s).next())) {
            (Some(p), Some(b)) => b.value().attr("href").and_then(|h| p.join(h).ok()).or(page.clone()),
            _ => page,
        };
        Dom { doc, base }
    }

    pub fn title(&self) -> Option<String> {
        let s = sel("title").ok()?;
        self.doc.select(&s).next().map(|e| squash(&e.text().collect::<String>()))
    }

    /// Visible text of each element matching `css`.
    pub fn select_text(&self, css: &str) -> Result<Vec<String>> {
        Ok(self.doc.select(&sel(css)?).map(visible_text).collect())
    }

    /// Outer HTML of each element matching `css`.
    pub fn select_html(&self, css: &str) -> Result<Vec<String>> {
        Ok(self.doc.select(&sel(css)?).map(|e| e.html()).collect())
    }

    /// Visible text of the whole page.
    pub fn text(&self) -> String {
        visible_text(self.doc.root_element())
    }

    /// Absolute http(s) link targets, in document order, deduplicated.
    pub fn links(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for a in self.doc.select(&sel("a[href]").unwrap()) {
            let href = a.value().attr("href").unwrap_or_default();
            let url = match &self.base {
                Some(b) => b.join(href).ok(),
                None => Url::parse(href).ok(),
            };
            if let Some(mut u) = url.filter(|u| matches!(u.scheme(), "http" | "https")) {
                u.set_fragment(None);
                let s = u.to_string();
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
        out
    }
}

fn sel(css: &str) -> Result<Selector> {
    Selector::parse(css).map_err(|e| ScoutError::Parse(format!("bad selector {css:?}: {e}")))
}

fn visible_text(el: ElementRef) -> String {
    let mut buf = String::new();
    for node in el.descendants() {
        if let Some(t) = node.value().as_text() {
            let hidden = node.ancestors().any(|a| {
                a.value().as_element().is_some_and(|e| HIDDEN.contains(&e.name()))
            });
            if !hidden {
                buf.push_str(t);
                buf.push(' ');
            }
        }
    }
    squash(&buf)
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<html><head><title> Hi
        there </title><style>p{}</style><base href="/docs/"></head>
        <body><p class=x>One <b>two</b></p><script>var a=1</script>
        <p class=x>three</p><a href="a#top">A</a><a href="a">dup</a>
        <a href="https://other.org/">B</a><a href="mailto:x@y">M</a><div><p>unclosed</div></body>"#;

    #[test]
    fn dom() {
        let d = Dom::parse(PAGE, "https://ex.com/start/page");
        assert_eq!(d.title().as_deref(), Some("Hi there"));
        assert_eq!(d.select_text("p.x").unwrap(), ["One two", "three"]);
        assert_eq!(d.select_html("b").unwrap(), ["<b>two</b>"]);
        assert_eq!(d.links(), ["https://ex.com/docs/a", "https://other.org/"]);
        let text = d.text();
        assert!(text.contains("unclosed") && !text.contains("var a") && !text.contains("p{}"));
        assert!(d.select_text("p[").is_err());
    }
}
