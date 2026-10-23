//! The visible text of a document in blocks, as a reader meets it:
//! headings, paragraphs, list items, cells. Two documents of one page (as
//! sent and rendered) compare block by block, which shows what JavaScript
//! added or took away; blocks a crawl met on many pages are the site's
//! template (menus, header, footer), told apart by `block_hash`.
//!
//! The blocks are read in the streaming pass that reads the facts, so a
//! crawl hashes every page's blocks for the price of the text it reads
//! anyway, and the viewer splits a document exactly as the crawl did.

use std::borrow::Cow;

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

/// The block element named `name`, if it is one.
pub(crate) fn block_tag(name: &str) -> Option<&'static str> {
    BLOCKS.iter().copied().find(|tag| *tag == name)
}

/// The identity of a block across pages: its element and text.
pub fn block_hash(tag: &str, text: &str) -> u64 {
    let mut hash = xxhash_rust::xxh3::Xxh3::new();
    hash.update(tag.as_bytes());
    hash.update(&[0]);
    hash.update(text.as_bytes());
    hash.digest()
}

/// An element the reader keeps open: a block, or one hidden by its markup
/// (`hidden`, `style="display:none"`) whose text is not shown.
struct Open {
    tag: Cow<'static, str>,
    block: bool,
    hidden: bool,
}

/// Blocks as the document is read: block elements open and close, text
/// arrives between. Text nodes side by side join as written, so
/// `<b>Hel</b>lo` is one word, and whitespace collapses when a block ends.
/// Keeps the blocks, or (for a crawl) only their hashes.
pub(crate) struct BlockReader {
    keep: bool,
    blocks: Vec<TextBlock>,
    hashes: Vec<u64>,
    text: String,
    collapsed: String,
    /// Open elements. One closed implicitly (`<li>` by the next `<li>`)
    /// stays until an element around it closes.
    open: Vec<Open>,
    /// Open elements that hide their text.
    hidden: usize,
}

impl BlockReader {
    pub(crate) fn new(keep: bool) -> Self {
        BlockReader {
            keep,
            blocks: Vec::new(),
            hashes: Vec::new(),
            text: String::new(),
            collapsed: String::new(),
            open: Vec::new(),
            hidden: 0,
        }
    }

    /// An element starts: a block (`block_tag`), or any element `hidden`.
    pub(crate) fn open(&mut self, tag: Cow<'static, str>, block: bool, hidden: bool) {
        if block {
            self.flush();
        }
        self.hidden += usize::from(hidden);
        self.open.push(Open { tag, block, hidden });
    }

    pub(crate) fn close(&mut self, tag: &str, block: bool) {
        if block {
            self.flush();
        }
        if let Some(at) = self.open.iter().rposition(|open| open.tag == tag) {
            self.open.truncate(at);
            self.hidden = self.open.iter().filter(|open| open.hidden).count();
        }
    }

    pub(crate) fn line_break(&mut self) {
        self.flush();
    }

    pub(crate) fn text(&mut self, text: &str) {
        if self.hidden == 0 {
            self.text.push_str(text);
        }
    }

    fn flush(&mut self) {
        if self.text.is_empty() {
            return;
        }
        self.collapsed.clear();
        for word in self.text.split_whitespace() {
            if !self.collapsed.is_empty() {
                self.collapsed.push(' ');
            }
            self.collapsed.push_str(word);
        }
        self.text.clear();
        if self.collapsed.is_empty() {
            return;
        }
        let tag = self.open.iter().rev().find(|open| open.block).map_or("body", |open| open.tag.as_ref());
        if self.keep {
            self.blocks.push(TextBlock { tag: tag.to_string(), text: self.collapsed.clone() });
        } else {
            self.hashes.push(block_hash(tag, &self.collapsed));
        }
    }

    /// The blocks in document order, or the hashes of the distinct ones.
    pub(crate) fn finish(mut self) -> (Vec<TextBlock>, Vec<u64>) {
        self.flush();
        self.hashes.sort_unstable();
        self.hashes.dedup();
        (self.blocks, self.hashes)
    }
}

/// The visible text of `html` in blocks, in document order.
pub fn text_blocks(html: &str) -> Vec<TextBlock> {
    crate::stream::read_blocks(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(html: &str) -> Vec<(String, String)> {
        text_blocks(html).into_iter().map(|block| (block.tag, block.text)).collect()
    }

    #[test]
    fn splits_text_into_the_blocks_a_reader_sees() {
        let blocks = pairs(
            "<html><head><title>T</title><style>p{}</style></head><body>
             <h1>Kettles  for <b>every</b> kitchen</h1>
             <div>Intro<p>First   paragraph</p>tail<br>after break</div>
             <ul><li>One</li><li>Two <a href=/x>link</a></li></ul>
             <script>var hidden = 1</script><noscript>Enable JS</noscript>
             <svg><text>chart</text></svg>
             </body></html>",
        );
        let expected = [
            ("h1", "Kettles for every kitchen"),
            ("div", "Intro"),
            ("p", "First paragraph"),
            ("div", "tail"),
            ("div", "after break"),
            ("li", "One"),
            ("li", "Two link"),
        ];
        assert_eq!(blocks, expected.map(|(tag, text)| (tag.to_string(), text.to_string())));
        let joined = text_blocks("<p><b>Hel</b>lo <i>world</i></p><p>8<span>9</span><span>6</span> 6</p>");
        assert_eq!(joined.iter().map(|block| block.text.as_str()).collect::<Vec<_>>(), ["Hello world", "896 6"]);
    }

    #[test]
    fn reads_markup_as_written() {
        // References resolved; list items and paragraphs closed implicitly.
        let blocks = pairs("<body><ul><li>Tea&nbsp;&amp; cake<li>Coffee</ul><p>One<p>Two</body>");
        let expected = [("li", "Tea & cake"), ("li", "Coffee"), ("p", "One"), ("p", "Two")];
        assert_eq!(blocks, expected.map(|(tag, text)| (tag.to_string(), text.to_string())));
        // After the list closes, its text is the page's again.
        assert_eq!(pairs("<div><ul><li>a<li>b</ul>after</div>").last().unwrap(), &("div".to_string(), "after".to_string()));
    }

    #[test]
    fn leaves_out_what_the_markup_hides() {
        let blocks = pairs(
            "<body><div id=prices style='display: none'>{\"a\":1}<p>inner</p></div>\
             <p>Price <span hidden>secret</span>9 900</p><section hidden><h2>Modal</h2></section><p>End</p></body>",
        );
        let expected = [("p", "Price 9 900"), ("p", "End")];
        assert_eq!(blocks, expected.map(|(tag, text)| (tag.to_string(), text.to_string())));
    }
}
