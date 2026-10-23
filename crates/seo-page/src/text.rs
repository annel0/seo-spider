//! The visible text of a document in blocks, as a reader meets it:
//! headings, paragraphs, list items, cells. Two documents of one page (as
//! sent and rendered) compare block by block, which shows what JavaScript
//! added or took away.

use ego_tree::NodeRef;
use scraper::{Html, Node};
use serde::Serialize;

/// One block of visible text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextBlock {
    /// The element that holds it: `h1`, `p`, `li`, `td`...
    pub tag: String,
    /// Its text, whitespace collapsed.
    pub text: String,
}

/// Elements that start a block of their own.
const BLOCKS: &[&str] = &[
    "address", "article", "aside", "blockquote", "caption", "dd", "details", "div", "dl", "dt", "fieldset", "figcaption",
    "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hr", "label", "legend", "li", "main", "nav",
    "ol", "option", "p", "pre", "section", "summary", "table", "td", "th", "tr", "ul", "button",
];

/// Elements whose text is not shown.
const HIDDEN: &[&str] = &["script", "style", "noscript", "template", "head", "title", "svg"];

struct Blocks {
    blocks: Vec<TextBlock>,
    text: String,
    tags: Vec<String>,
    /// Whitespace came after the last word: the next one is a new word.
    /// Text nodes side by side without it are one word (`<b>Hel</b>lo`,
    /// a phone number set a digit a `<span>`).
    space: bool,
}

impl Blocks {
    fn flush(&mut self) {
        self.space = false;
        if self.text.is_empty() {
            return;
        }
        let tag = self.tags.last().cloned().unwrap_or_else(|| "body".into());
        self.blocks.push(TextBlock { tag, text: std::mem::take(&mut self.text) });
    }

    fn push(&mut self, text: &str) {
        if text.starts_with(char::is_whitespace) {
            self.space = true;
        }
        for word in text.split_whitespace() {
            if self.space && !self.text.is_empty() {
                self.text.push(' ');
            }
            self.text.push_str(word);
            self.space = true;
        }
        self.space = text.ends_with(char::is_whitespace) || (self.space && text.trim().is_empty());
    }

    fn walk(&mut self, node: NodeRef<'_, Node>) {
        match node.value() {
            Node::Text(text) => self.push(text),
            Node::Element(element) => {
                let name = element.name();
                if HIDDEN.contains(&name) {
                    return;
                }
                if name == "br" {
                    self.flush();
                    return;
                }
                let block = BLOCKS.contains(&name);
                if block {
                    self.flush();
                    self.tags.push(name.to_string());
                }
                for child in node.children() {
                    self.walk(child);
                }
                if block {
                    self.flush();
                    self.tags.pop();
                }
            }
            _ => {
                for child in node.children() {
                    self.walk(child);
                }
            }
        }
    }
}

/// The visible text of `html` in blocks, in document order.
pub fn text_blocks(html: &str) -> Vec<TextBlock> {
    let document = Html::parse_document(html);
    let mut blocks = Blocks { blocks: Vec::new(), text: String::new(), tags: Vec::new(), space: false };
    blocks.walk(document.tree.root());
    blocks.flush();
    blocks.blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_text_into_the_blocks_a_reader_sees() {
        let blocks = text_blocks(
            "<html><head><title>T</title><style>p{}</style></head><body>
             <h1>Kettles  for <b>every</b> kitchen</h1>
             <div>Intro<p>First   paragraph</p>tail<br>after break</div>
             <ul><li>One</li><li>Two <a href=/x>link</a></li></ul>
             <script>var hidden = 1</script><noscript>Enable JS</noscript>
             </body></html>",
        );
        let pairs: Vec<(&str, &str)> = blocks.iter().map(|block| (block.tag.as_str(), block.text.as_str())).collect();
        assert_eq!(
            pairs,
            [
                ("h1", "Kettles for every kitchen"),
                ("div", "Intro"),
                ("p", "First paragraph"),
                ("div", "tail"),
                ("div", "after break"),
                ("li", "One"),
                ("li", "Two link"),
            ]
        );
        let joined = text_blocks("<p><b>Hel</b>lo <i>world</i></p><p>8<span>9</span><span>6</span> 6</p>");
        assert_eq!(joined.iter().map(|block| block.text.as_str()).collect::<Vec<_>>(), ["Hello world", "896 6"]);
    }
}
