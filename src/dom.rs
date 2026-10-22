use scraper::{ElementRef, Html, Selector};
use serde::Serialize;
use serde_json::{Map, Value};
use url::Url;

use crate::error::{Result, ScoutError};

/// Tags whose text is never shown to a reader.
const HIDDEN: &[&str] = &["script", "style", "noscript", "template", "head"];

#[derive(Debug, Serialize)]
pub struct Link {
    pub href: String,
    pub text: String,
    pub rel: Option<String>,
    pub is_external: bool,
}

#[derive(Debug, Serialize)]
pub struct Form {
    pub action: Option<String>,
    pub method: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Serialize)]
pub struct Field {
    pub tag: String,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub value: Option<String>,
}

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

    /// Absolute http(s) links, in document order, deduplicated by href.
    pub fn links(&self) -> Vec<Link> {
        let mut out: Vec<Link> = Vec::new();
        for a in self.doc.select(&sel("a[href]").unwrap()) {
            let Some(mut u) = self.resolve(a.value().attr("href").unwrap_or_default())
                .filter(|u| matches!(u.scheme(), "http" | "https"))
            else {
                continue;
            };
            u.set_fragment(None);
            let href = u.to_string();
            if out.iter().any(|l| l.href == href) {
                continue;
            }
            out.push(Link {
                is_external: self.base.as_ref().is_some_and(|b| b.host_str() != u.host_str()),
                href,
                text: visible_text(a),
                rel: a.value().attr("rel").map(str::to_string),
            });
        }
        out
    }

    pub fn forms(&self) -> Vec<Form> {
        let fields = sel("input, select, textarea, button").unwrap();
        self.doc.select(&sel("form").unwrap()).map(|f| Form {
            action: f.value().attr("action").and_then(|a| self.resolve(a)).map(|u| u.to_string()),
            method: f.value().attr("method").unwrap_or("get").to_ascii_lowercase(),
            fields: f.select(&fields).map(|e| Field {
                tag: e.value().name().to_string(),
                name: e.value().attr("name").map(str::to_string),
                kind: e.value().attr("type").map(str::to_string),
                value: e.value().attr("value").map(str::to_string),
            }).collect(),
        }).collect()
    }

    /// Title, <meta> name/property → content, and parsed JSON-LD blocks.
    pub fn metadata(&self) -> Value {
        let mut meta = Map::new();
        for m in self.doc.select(&sel("meta[content]").unwrap()) {
            let v = m.value();
            if let Some(k) = v.attr("property").or(v.attr("name")).or(v.attr("itemprop")) {
                meta.insert(k.to_string(), v.attr("content").unwrap_or_default().into());
            }
        }
        let json_ld: Vec<Value> = self.doc
            .select(&sel(r#"script[type="application/ld+json"]"#).unwrap())
            .filter_map(|s| serde_json::from_str(&s.text().collect::<String>()).ok())
            .collect();
        serde_json::json!({
            "title": self.title(),
            "canonical": self.doc.select(&sel(r#"link[rel="canonical"][href]"#).unwrap()).next()
                .and_then(|l| self.resolve(l.value().attr("href")?)).map(|u| u.to_string()),
            "meta": meta,
            "json_ld": json_ld,
        })
    }

    fn resolve(&self, href: &str) -> Option<Url> {
        match &self.base {
            Some(b) => b.join(href).ok(),
            None => Url::parse(href).ok(),
        }
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
        let links: Vec<_> = d.links().into_iter().map(|l| (l.href, l.is_external)).collect();
        assert_eq!(links, [("https://ex.com/docs/a".into(), false), ("https://other.org/".into(), true)]);
        let text = d.text();
        assert!(text.contains("unclosed") && !text.contains("var a") && !text.contains("p{}"));
        assert!(d.select_text("p[").is_err());
    }

    #[test]
    fn forms_and_metadata() {
        let d = Dom::parse(r#"<head><meta property="og:title" content="OG">
            <script type="application/ld+json">{"@type":"Thing"}</script></head>
            <form action="/go" method=POST><input name=q value=hi><select name=s></select></form>"#,
            "https://ex.com/x");
        let f = &d.forms()[0];
        assert_eq!((f.action.as_deref(), f.method.as_str()), (Some("https://ex.com/go"), "post"));
        assert_eq!(f.fields.len(), 2);
        assert_eq!(f.fields[0].value.as_deref(), Some("hi"));
        let m = d.metadata();
        assert_eq!(m["meta"]["og:title"], "OG");
        assert_eq!(m["json_ld"][0]["@type"], "Thing");
    }
}
