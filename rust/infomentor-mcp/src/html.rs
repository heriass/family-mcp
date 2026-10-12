//! HTML reading for packages/infomentor-mcp/src/http.ts's `parseForms` and `parseParent`: the
//! WHATWG tokenizer of html5gum, under htmlparser2 12's tree rules in HTML mode (implied closes,
//! void elements, an ignored nested form, foreign content, end-of-input closes), so the same open,
//! close and text callbacks arrive. Nothing here runs or evaluates page content.

use std::collections::HashSet;

use html5gum::emitters::callback::{CallbackEmitter, CallbackEvent};
use html5gum::{Span, State, Tokenizer};

use crate::js;

/// One tokenizer event, owned.
enum Event {
    Open(String),
    Attribute(String),
    Value(String),
    Close { self_closing: bool },
    End(String),
    Text(String),
}

/// htmlparser2's callbacks: `onopentag`, `onclosetag` and `ontext`.
trait Handler {
    fn open(&mut self, _name: &str, _attributes: &[(String, String)]) {}
    fn close(&mut self, _name: &str) {}
    fn text(&mut self, _text: &str) {}
}

const FORM_TAGS: &[&str] = &[
    "input", "option", "optgroup", "select", "button", "datalist", "textarea",
];

const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6", "p"];

/// htmlparser2's `openImpliesClose`: opening a key closes these while one is the current element.
fn implied_closes(name: &str) -> &'static [&'static str] {
    match name {
        "tr" => &["tr", "th", "td"],
        "th" => &["th"],
        "td" => &["thead", "th", "td"],
        "body" => &["head", "link", "script"],
        "a" => &["a"],
        "li" => &["li"],
        "p" | "address" | "article" | "aside" | "blockquote" | "details" | "div" | "dl"
        | "fieldset" | "figcaption" | "figure" | "footer" | "form" | "header" | "hr" | "main"
        | "nav" | "ol" | "pre" | "section" | "table" | "ul" => &["p"],
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => HEADINGS,
        "select" | "input" | "output" | "button" | "datalist" | "textarea" => FORM_TAGS,
        "option" => &["option"],
        "optgroup" => &["optgroup", "option"],
        "dd" | "dt" => &["dd", "dt"],
        "rt" | "rp" => &["rt", "rp"],
        "tbody" | "tfoot" => &["thead", "tbody"],
        _ => &[],
    }
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "basefont"
            | "br"
            | "col"
            | "command"
            | "embed"
            | "frame"
            | "hr"
            | "img"
            | "input"
            | "isindex"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_integration(name: &str) -> bool {
    matches!(
        name,
        "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml" | "foreignObject" | "desc" | "title"
    )
}

/// htmlparser2's `svgTagNameAdjustments`.
fn svg_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "altglyph" => "altGlyph",
        "altglyphdef" => "altGlyphDef",
        "altglyphitem" => "altGlyphItem",
        "animatecolor" => "animateColor",
        "animatemotion" => "animateMotion",
        "animatetransform" => "animateTransform",
        "clippath" => "clipPath",
        "feblend" => "feBlend",
        "fecolormatrix" => "feColorMatrix",
        "fecomponenttransfer" => "feComponentTransfer",
        "fecomposite" => "feComposite",
        "feconvolvematrix" => "feConvolveMatrix",
        "fediffuselighting" => "feDiffuseLighting",
        "fedisplacementmap" => "feDisplacementMap",
        "fedistantlight" => "feDistantLight",
        "fedropshadow" => "feDropShadow",
        "feflood" => "feFlood",
        "fefunca" => "feFuncA",
        "fefuncb" => "feFuncB",
        "fefuncg" => "feFuncG",
        "fefuncr" => "feFuncR",
        "fegaussianblur" => "feGaussianBlur",
        "feimage" => "feImage",
        "femerge" => "feMerge",
        "femergenode" => "feMergeNode",
        "femorphology" => "feMorphology",
        "feoffset" => "feOffset",
        "fepointlight" => "fePointLight",
        "fespecularlighting" => "feSpecularLighting",
        "fespotlight" => "feSpotLight",
        "fetile" => "feTile",
        "feturbulence" => "feTurbulence",
        "foreignobject" => "foreignObject",
        "glyphref" => "glyphRef",
        "lineargradient" => "linearGradient",
        "radialgradient" => "radialGradient",
        "textpath" => "textPath",
        _ => return None,
    })
}

/// The tokenizer state after a text-only start tag outside foreign content.
fn text_state(name: &str) -> Option<State> {
    match name {
        "script" => Some(State::ScriptData),
        "style" | "xmp" | "iframe" | "noembed" | "noframes" => Some(State::RawText),
        "title" | "textarea" => Some(State::RcData),
        "plaintext" => Some(State::PlainText),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Foreign {
    None,
    Svg,
    MathMl,
}

/// The start tag being read: its name ("" for an ignored nested form) and first attributes.
struct Pending {
    name: String,
    attributes: Vec<(String, String)>,
    /// The names in `attributes`, so a duplicate is found in constant time however many a tag has.
    seen: HashSet<String>,
    /// The attribute a value belongs to; `None` after a duplicate name.
    current: Option<usize>,
    /// Whether the tag began in foreign content, where no tag is text-only.
    foreign: bool,
    raw: String,
}

/// htmlparser2's `Parser` stack, foreign-context stack and callbacks.
struct Tree<'h> {
    stack: Vec<String>,
    foreign: Vec<Foreign>,
    handler: &'h mut dyn Handler,
}

impl Tree<'_> {
    fn in_foreign(&self) -> bool {
        self.foreign.last() != Some(&Foreign::None)
    }

    /// `readTagName` for an already lowercased name.
    fn tag_name(&self, name: &str) -> String {
        if self.foreign.last() == Some(&Foreign::Svg) {
            return svg_name(name).unwrap_or(name).to_owned();
        }

        if self.foreign.len() > 1
            && let Some(adjusted) = svg_name(name)
            && self.stack.iter().any(|open| open == adjusted)
        {
            return adjusted.to_owned();
        }

        match (self.in_foreign(), name) {
            (false, "image") => "img".to_owned(),
            _ => name.to_owned(),
        }
    }

    fn pop(&mut self) {
        let Some(element) = self.stack.pop() else {
            return;
        };

        if matches!(element.as_str(), "math" | "svg") || is_integration(&element) {
            self.foreign.pop();
        }
        self.handler.close(&element);
    }

    /// `emitOpenTag`: "" when a form is already open, as the spec ignores a nested one.
    fn emit_open(&mut self, name: String) -> String {
        if name == "form" && self.stack.iter().any(|open| open == "form") {
            return String::new();
        }
        let closes = implied_closes(&name);

        while self
            .stack
            .last()
            .is_some_and(|top| closes.contains(&top.as_str()))
        {
            self.pop();
        }

        if !is_void(&name) {
            self.stack.push(name.clone());

            match name.as_str() {
                "svg" => self.foreign.push(Foreign::Svg),
                "math" => self.foreign.push(Foreign::MathMl),
                _ if is_integration(&name) => self.foreign.push(Foreign::None),
                _ => {}
            }
        }
        name
    }

    /// `endOpenTag`: the open callback, then the close of a void element.
    fn end_open(&mut self, name: &str, attributes: &[(String, String)]) {
        if name.is_empty() {
            return;
        }
        self.handler.open(name, attributes);

        if is_void(name) {
            self.handler.close(name);
        }
    }

    /// `onclosetag`.
    fn end_tag(&mut self, raw: &str) {
        let name = self.tag_name(raw);

        if !is_void(&name) {
            if let Some(at) = self.stack.iter().rposition(|open| *open == name) {
                while self.stack.len() > at {
                    self.pop();
                }
            } else if name == "p" {
                let name = self.emit_open(name);
                self.end_open(&name, &[]);

                if self.stack.last() == Some(&name) {
                    self.pop();
                }
            }
        } else if name == "br" {
            self.handler.open("br", &[]);
            self.handler.close("br");
        }
    }
}

/// Run `html` through the tokenizer and the tree rules.
fn parse(html: &str, handler: &mut dyn Handler) {
    let emitter = CallbackEmitter::new(|event: CallbackEvent<'_>, _: Span<()>| {
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        Some(match event {
            CallbackEvent::OpenStartTag { name } => Event::Open(text(name)),
            CallbackEvent::AttributeName { name } => Event::Attribute(text(name)),
            CallbackEvent::AttributeValue { value } => Event::Value(text(value)),
            CallbackEvent::CloseStartTag { self_closing } => Event::Close { self_closing },
            CallbackEvent::EndTag { name } => Event::End(text(name)),
            CallbackEvent::String { value } => Event::Text(text(value)),
            _ => return None,
        })
    });
    let mut tokenizer = Tokenizer::new_with_emitter(html, emitter);
    let mut tree = Tree {
        stack: Vec::new(),
        foreign: vec![Foreign::None],
        handler,
    };
    let mut pending: Option<Pending> = None;

    // Each event arrives before the tokenizer reads past it, so a text-only start tag switches
    // the state before its content is read.
    while let Some(Ok(event)) = tokenizer.next() {
        match event {
            Event::Open(raw) => {
                let foreign = tree.in_foreign();
                let name = tree.tag_name(&raw);
                let name = tree.emit_open(name);
                pending = Some(Pending {
                    name,
                    attributes: Vec::new(),
                    seen: HashSet::new(),
                    current: None,
                    foreign,
                    raw,
                });
            }
            // An end tag's attributes are read and dropped.
            Event::Attribute(name) => {
                if let Some(tag) = &mut pending {
                    // The first of duplicate attributes wins.
                    tag.current = match tag.seen.insert(name.clone()) {
                        false => None,
                        true => {
                            tag.attributes.push((name, String::new()));
                            Some(tag.attributes.len() - 1)
                        }
                    };
                }
            }
            Event::Value(value) => {
                if let Some(Pending {
                    attributes,
                    current: Some(at),
                    ..
                }) = &mut pending
                {
                    attributes[*at].1 = value;
                }
            }
            Event::Close { self_closing } => {
                let Some(tag) = pending.take() else {
                    continue;
                };

                // Self-closing is honoured only in foreign content.
                if self_closing && tree.in_foreign() {
                    tree.end_open(&tag.name, &tag.attributes);

                    if !tag.name.is_empty() && tree.stack.last() == Some(&tag.name) {
                        tree.pop();
                    }
                } else {
                    tree.end_open(&tag.name, &tag.attributes);
                }

                if !tag.foreign
                    && let Some(state) = text_state(&tag.raw)
                {
                    tokenizer.set_state(state);
                }
            }
            Event::End(name) => {
                pending = None;
                tree.end_tag(&name);
            }
            Event::Text(text) => tree.handler.text(&text),
        }
    }

    // `onend`: every element still open is closed, innermost first.
    while let Some(element) = tree.stack.pop() {
        tree.handler.close(&element);
    }
}

/// `Form`: `fields` keeps the hidden inputs in document order, as `URLSearchParams` does.
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    pub id: String,
    pub action: String,
    pub method: String,
    pub fields: Vec<(String, String)>,
}

impl Form {
    pub fn has(&self, name: &str) -> bool {
        self.fields.iter().any(|(field, _)| field == name)
    }

    /// `URLSearchParams.set`: the first field of that name takes the value, later ones go; a new
    /// name is appended.
    pub fn set(&mut self, name: &str, value: &str) {
        let mut seen = false;
        self.fields.retain_mut(|(field, current)| {
            if field != name {
                return true;
            }

            if seen {
                return false;
            }
            seen = true;
            *current = value.to_owned();
            true
        });

        if !seen {
            self.fields.push((name.to_owned(), value.to_owned()));
        }
    }
}

fn attribute<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(known, _)| known == name)
        .map(|(_, value)| value.as_str())
}

/// `parseForms`: each form with its enabled hidden inputs.
pub fn parse_forms(html: &str) -> Vec<Form> {
    #[derive(Default)]
    struct Forms {
        forms: Vec<Form>,
        open: bool,
    }

    impl Handler for Forms {
        fn open(&mut self, name: &str, attributes: &[(String, String)]) {
            let get = |name| attribute(attributes, name);

            if name == "form" {
                self.forms.push(Form {
                    id: get("id").unwrap_or_default().to_owned(),
                    action: get("action").unwrap_or_default().to_owned(),
                    method: get("method").unwrap_or("get").to_lowercase(),
                    fields: Vec::new(),
                });
                self.open = true;
            } else if name == "input"
                && self.open
                && let Some(field) = get("name").filter(|field| !field.is_empty())
                && get("type").is_some_and(|kind| kind.to_lowercase() == "hidden")
                && get("disabled").is_none()
                && let Some(form) = self.forms.last_mut()
            {
                form.fields.push((
                    field.to_owned(),
                    get("value").unwrap_or_default().to_owned(),
                ));
            }
        }

        fn close(&mut self, name: &str) {
            if name == "form" {
                self.open = false;
            }
        }
    }

    let mut forms = Forms::default();
    parse(html, &mut forms);
    forms.forms
}

/// Plain school content, preserving paragraphs, list items and safe link destinations.
/// Hidden/script/form contents and HTML attributes never cross the tool boundary.
pub fn plain_text(html: &str) -> String {
    #[derive(Default)]
    struct Text {
        out: String,
        hidden: usize,
        links: Vec<Option<String>>,
    }
    impl Handler for Text {
        fn open(&mut self, name: &str, attrs: &[(String, String)]) {
            if matches!(name, "script" | "style" | "form" | "noscript") { self.hidden += 1; }
            if self.hidden != 0 { return; }
            if matches!(name, "p" | "div" | "br" | "li" | "h1" | "h2" | "h3" | "tr") { self.out.push('\n'); }
            if name == "li" { self.out.push_str("- "); }
            if name == "a" {
                let href = attribute(attrs, "href").and_then(|href| {
                    let url = url::Url::parse(crate::session::PARENT_URL).ok()?.join(href).ok()?;
                    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() { return None; }
                    let mut url = url;
                    url.set_query(None); url.set_fragment(None);
                    Some(url.to_string())
                });
                self.links.push(href);
            }
        }
        fn close(&mut self, name: &str) {
            if matches!(name, "script" | "style" | "form" | "noscript") { self.hidden = self.hidden.saturating_sub(1); return; }
            if self.hidden != 0 { return; }
            if name == "a" && let Some(Some(href)) = self.links.pop() { self.out.push_str(&format!(" ({href})")); }
            if matches!(name, "p" | "div" | "li" | "h1" | "h2" | "h3" | "tr") { self.out.push('\n'); }
        }
        fn text(&mut self, text: &str) { if self.hidden == 0 { self.out.push_str(text); } }
    }
    let mut text = Text::default();
    parse(html, &mut text);
    text.out.lines().map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n")
}

/// The capture of `/IMHome\.home\.homeData\s*=\s*([\s\S]*?);\s*IMHome\.home\.init\(/`.
fn home_data(script: &str) -> Option<&str> {
    const DATA: &str = "IMHome.home.homeData";
    const INIT: &str = "IMHome.home.init(";

    for (at, _) in script.match_indices(DATA) {
        let rest = script[at + DATA.len()..].trim_start_matches(js::is_space);
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let value = rest.trim_start_matches(js::is_space);

        // The shortest capture that the terminator follows.
        return value.match_indices(';').find_map(|(end, _)| {
            value[end + 1..]
                .trim_start_matches(js::is_space)
                .starts_with(INIT)
                .then(|| &value[..end])
        });
    }
    None
}

/// `parseParent`'s script reading: each script's `IMHome.home.homeData` text, in order. The
/// caller parses each and keeps the last that matches its schema.
pub fn parent_scripts(html: &str) -> Vec<String> {
    #[derive(Default)]
    struct Scripts {
        open: bool,
        script: String,
        found: Vec<String>,
    }

    impl Handler for Scripts {
        fn open(&mut self, name: &str, _: &[(String, String)]) {
            if name == "script" {
                self.open = true;
                self.script.clear();
            }
        }

        fn text(&mut self, text: &str) {
            if self.open {
                self.script.push_str(text);
            }
        }

        fn close(&mut self, name: &str) {
            if name != "script" {
                return;
            }
            self.open = false;

            if let Some(data) = home_data(&self.script).filter(|data| !data.is_empty()) {
                self.found.push(data.to_owned());
            }
        }
    }

    let mut scripts = Scripts::default();
    parse(html, &mut scripts);
    scripts.found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Page text is untrusted: one tag with very many attributes must not stall a call. The first
    /// of duplicate attributes still wins.
    #[test]
    fn a_tag_with_very_many_attributes_is_read_quickly() {
        let attributes: String = (0..100_000).map(|index| format!(" a{index}=1")).collect();
        let started = std::time::Instant::now();
        let forms = parse_forms(&format!(
            "<form id=first{attributes} id=second><input type=hidden name=n value=v{attributes} value=w></form>"
        ));
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0].id, "first");
        assert_eq!(forms[0].fields, [("n".to_owned(), "v".to_owned())]);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    fn fields(form: &Form) -> Vec<(&str, &str)> {
        form.fields
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect()
    }

    #[test]
    fn forms_keep_enabled_hidden_inputs_in_order() {
        let forms = parse_forms(
            r#"<FORM id=login ACTION="/x?a=1&amp;b=2" Method=POST>
                <input type=HIDDEN name=__VIEWSTATE value="v&lt;1" value="ignored">
                <input type=hidden name=disabled value=1 disabled>
                <input type=text name=visible value=1>
                <input type=hidden name=empty>
                <input type=hidden value=nameless>
                <form id=nested><input type=hidden name=inner value=2></form>
                <input type=hidden name=after value=3>
              </form>
              <input type=hidden name=outside value=4>
              <form><div><input type="hidden" name="a" value="1"></div></form>
              <script>"<form id=fake><input type=hidden name=s value=1>"</script>
              <textarea><form id=fake2></textarea>"#,
        );
        assert_eq!(forms.len(), 2);
        assert_eq!(
            (
                forms[0].id.as_str(),
                forms[0].action.as_str(),
                forms[0].method.as_str()
            ),
            ("login", "/x?a=1&b=2", "post")
        );
        // The nested form is ignored, so its `</form>` closes the outer one.
        assert_eq!(
            fields(&forms[0]),
            [("__VIEWSTATE", "v<1"), ("empty", ""), ("inner", "2")]
        );
        assert_eq!(forms[1].method, "get");
        assert_eq!(fields(&forms[1]), [("a", "1")]);
    }

    #[test]
    fn ancestors_and_implied_closes_end_forms_as_htmlparser2_does() {
        // `</div>` closes the form inside it.
        let forms = parse_forms(
            "<div><form><input type=hidden name=a value=1></div><input type=hidden name=b value=2>",
        );
        assert_eq!(fields(&forms[0]), [("a", "1")]);
        // Opening the form closes the paragraph, so `</p>` is an implicit `<p></p>` and the form
        // stays open.
        let forms = parse_forms(
            "<p><form></p><input type=hidden name=a value=1><svg><form/></svg><input type=hidden name=b value=2>",
        );
        assert_eq!(fields(&forms[0]), [("a", "1"), ("b", "2")]);
        // Unclosed at the end of input.
        let forms = parse_forms("<form><input type=hidden name=a value=1>");
        assert_eq!(fields(&forms[0]), [("a", "1")]);
    }

    #[test]
    fn form_fields_set_like_url_search_params() {
        let mut form = Form {
            id: String::new(),
            action: String::new(),
            method: "post".to_owned(),
            fields: vec![
                ("a".to_owned(), "1".to_owned()),
                ("b".to_owned(), "2".to_owned()),
                ("a".to_owned(), "3".to_owned()),
            ],
        };
        form.set("a", "x");
        form.set("c", "y");
        assert_eq!(fields(&form), [("a", "x"), ("b", "2"), ("c", "y")]);
    }

    #[test]
    fn parent_data_is_read_from_script_text_only() {
        let page = r#"<html><body>
            <p>IMHome.home.homeData = {"text": 1}; IMHome.home.init(</p>
            <script>var x = "</scrip>"; IMHome.home.homeData =
              {"a": "; IMHome.home.init"} ;
              IMHome.home.init({});</script>
            <SCRIPT>IMHome.home.homeData = ;IMHome.home.init(1)</SCRIPT>
            <script>IMHome.home.homeData = [1]; IMHome.home.init(2)"#;
        assert_eq!(
            parent_scripts(page),
            [r#"{"a": "; IMHome.home.init"} "#, "[1]"]
        );
        assert!(home_data("IMHome.home.homeData x = 1; IMHome.home.init(").is_none());
        assert_eq!(
            home_data(
                "IMHome.home.homeData; IMHome.home.homeData\u{a0}=\n2;\u{2028}IMHome.home.init("
            ),
            Some("2")
        );
    }
}
