// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

//! The tree the browser builds out of a document, from html5ever, Servo's spec-compliant parser: a
//! second oracle, which knows where an element really ends up rather than where its tag is.
//!
//! html5ever reports no source positions. The document is fed to it one piece at a time, split after
//! every `>`, and each node records where the input stood when the parser created it: an element
//! made for a start tag records the end of that tag. Nodes are numbered in the order they are
//! created, which tells what the parser added to the head after it closed it.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{ns, parse_document, Attribute, ParseOpts, QualName};

use super::oracle;

/// A node of the tree.
#[derive(Debug)]
pub struct Node {
    /// Creation order, the document being 0.
    pub id: usize,
    /// Where the input stood when the parser created the node, in the bytes of the document.
    pub offset: usize,
    pub kind: Kind,
    /// An element's attributes, in source order.
    pub attributes: RefCell<Vec<Attribute>>,
    /// A text node's or a comment's content.
    pub text: RefCell<String>,
    pub parent: RefCell<Option<Weak<Node>>>,
    pub children: RefCell<Vec<Handle>>,
    /// The number of nodes created when the parser first popped the element off its stack of open
    /// elements: the ones it created from then on, it did not put in it while it was open.
    pub popped: Cell<Option<usize>>,
    /// Where the input stood then.
    pub popped_at: Cell<Option<usize>>,
}

pub type Handle = Rc<Node>;

#[derive(Debug)]
pub enum Kind {
    Document,
    Doctype {
        name: String,
        public_id: String,
        system_id: String,
    },
    Element {
        name: QualName,
        /// A template's contents, a fragment of its own.
        template_contents: Option<Handle>,
    },
    /// A template's contents.
    Fragment,
    Text,
    Comment,
}

impl Node {
    pub fn name(&self) -> Option<&QualName> {
        return match &self.kind {
            Kind::Element { name, .. } => Some(name),
            _ => None,
        };
    }

    /// Whether this is an element of the HTML namespace named `local`.
    pub fn is_html(&self, local: &str) -> bool {
        return self
            .name()
            .is_some_and(|name| return name.ns == ns!(html) && &*name.local == local);
    }

    /// The value of the attribute named `local`, in no namespace.
    pub fn attribute(&self, local: &str) -> Option<String> {
        return self
            .attributes
            .borrow()
            .iter()
            .find(|attribute| return attribute.name.ns == ns!() && &*attribute.name.local == local)
            .map(|attribute| return String::from(&*attribute.value));
    }

    pub fn parent(&self) -> Option<Handle> {
        return self.parent.borrow().as_ref().and_then(Weak::upgrade);
    }

    pub fn children(&self) -> Vec<Handle> {
        return self.children.borrow().clone();
    }

    /// Whether this is a text node holding nothing but ASCII whitespace.
    pub fn is_blank_text(&self) -> bool {
        return matches!(self.kind, Kind::Text)
            && self
                .text
                .borrow()
                .bytes()
                .all(|b| return b.is_ascii_whitespace());
    }

    /// Whether the node is within a template's contents, which are inert.
    pub fn in_template(&self) -> bool {
        let mut parent = self.parent();
        while let Some(node) = parent {
            if matches!(node.kind, Kind::Fragment) {
                return true;
            }
            parent = node.parent();
        }
        return false;
    }
}

/// What html5ever built out of a document.
pub struct Tree {
    pub document: Handle,
    /// How many nodes were created.
    pub count: usize,
}

impl Tree {
    /// The root element, the document's first element child.
    pub fn root(&self) -> Option<Handle> {
        return self
            .document
            .children()
            .into_iter()
            .find(|node| return node.name().is_some());
    }

    /// The document's head, as the DOM's `document.head` has it: the first `<head>` child of the
    /// `<html>` root element.
    pub fn head(&self) -> Option<Handle> {
        let root = self.root().filter(|root| return root.is_html("html"))?;
        return root
            .children()
            .into_iter()
            .find(|node| return node.is_html("head"));
    }

    /// Every node, template contents included, in tree order.
    pub fn nodes(&self) -> Vec<Handle> {
        let mut nodes = Vec::with_capacity(self.count);
        let mut stack = vec![self.document.clone()];
        while let Some(node) = stack.pop() {
            nodes.push(node.clone());
            let mut children = node.children();
            if let Kind::Element {
                template_contents: Some(contents),
                ..
            } = &node.kind
            {
                children.insert(0, contents.clone());
            }
            stack.extend(children.into_iter().rev());
        }
        return nodes;
    }

    /// Every element having an attribute named `local`, template contents included, in tree order.
    pub fn elements_with(&self, local: &str) -> Vec<Handle> {
        return self
            .nodes()
            .into_iter()
            .filter(|node| return node.attribute(local).is_some())
            .collect();
    }

    /// The tree in the format of the html5lib-tests tree construction tests.
    pub fn serialize(&self, skip: Option<&Handle>) -> String {
        return self.serialize_with(Options {
            skip,
            significant: false,
        });
    }

    /// [`Tree::serialize`] without `skip` and its subtree, with the text nodes the removal leaves
    /// adjacent merged, and without what the document, the `<html>` and the `<head>` elements hold
    /// that renders nothing, comments and text holding nothing but whitespace: where the parser
    /// starts the head decides whether it drops whitespace before it, and whether a comment goes in
    /// the head or ahead of it.
    pub fn serialize_significant(&self, skip: Option<&Handle>) -> String {
        return self.serialize_with(Options {
            skip,
            significant: true,
        });
    }

    fn serialize_with(&self, options: Options) -> String {
        let mut out = String::new();
        for child in merged_children(&self.document, options) {
            serialize_node(&mut out, &child, 0, options);
        }
        return out;
    }
}

#[derive(Clone, Copy)]
struct Options<'a> {
    skip: Option<&'a Handle>,
    significant: bool,
}

/// A child of a node, as the serialization sees it: text nodes adjacent once `skip` is removed are
/// one.
enum Child {
    Node(Handle),
    Text(String),
}

fn merged_children(node: &Handle, options: Options) -> Vec<Child> {
    let mut children: Vec<Child> = Vec::new();
    for child in node.children() {
        if options
            .skip
            .is_some_and(|skip| return Rc::ptr_eq(skip, &child))
        {
            continue;
        }
        if matches!(child.kind, Kind::Text) {
            let text = child.text.borrow();
            if let Some(Child::Text(last)) = children.last_mut() {
                last.push_str(&text);
            } else {
                children.push(Child::Text(text.clone()));
            }
            continue;
        }
        children.push(Child::Node(child));
    }
    if options.significant
        && (matches!(node.kind, Kind::Document) || node.is_html("html") || node.is_html("head"))
    {
        children.retain(|child| {
            return match child {
                Child::Text(text) => !text.bytes().all(|b| return b.is_ascii_whitespace()),
                Child::Node(node) => !matches!(node.kind, Kind::Comment),
            };
        });
    }
    return children;
}

fn serialize_node(out: &mut String, child: &Child, depth: usize, options: Options) {
    let indent = "  ".repeat(depth);
    let node = match child {
        Child::Text(text) => {
            out.push_str(&format!("| {indent}\"{text}\"\n"));
            return;
        }
        Child::Node(node) => node,
    };
    match &node.kind {
        Kind::Document | Kind::Fragment | Kind::Text => unreachable!("not a child"),
        Kind::Comment => out.push_str(&format!("| {indent}<!-- {} -->\n", node.text.borrow())),
        Kind::Doctype {
            name,
            public_id,
            system_id,
        } => match public_id.is_empty() && system_id.is_empty() {
            true => out.push_str(&format!("| {indent}<!DOCTYPE {name}>\n")),
            false => out.push_str(&format!(
                "| {indent}<!DOCTYPE {name} \"{public_id}\" \"{system_id}\">\n"
            )),
        },
        Kind::Element {
            name,
            template_contents,
        } => {
            let prefix = match name.ns {
                ns!(svg) => "svg ",
                ns!(mathml) => "math ",
                _ => "",
            };
            out.push_str(&format!("| {indent}<{prefix}{}>\n", name.local));
            let mut attributes: Vec<(String, String)> = node
                .attributes
                .borrow()
                .iter()
                .map(|attribute| {
                    // The parser gives `xmlns` an empty prefix.
                    let name = match attribute
                        .name
                        .prefix
                        .as_ref()
                        .filter(|p| return !p.is_empty())
                    {
                        Some(prefix) => format!("{prefix} {}", attribute.name.local),
                        None => String::from(&*attribute.name.local),
                    };
                    return (name, String::from(&*attribute.value));
                })
                .collect();
            // By UTF-16 code units, as the tests sort them.
            attributes.sort_by(|(a, _), (b, _)| return a.encode_utf16().cmp(b.encode_utf16()));
            for (name, value) in attributes {
                out.push_str(&format!("| {indent}  {name}=\"{value}\"\n"));
            }
            if let Some(contents) = template_contents {
                out.push_str(&format!("| {indent}  content\n"));
                for child in merged_children(contents, options) {
                    serialize_node(out, &child, depth + 2, options);
                }
            }
            for child in merged_children(node, options) {
                serialize_node(out, &child, depth + 1, options);
            }
        }
    }
}

/// Parses `document` as the browser does when it is UTF-8: decoded with a replacement character for
/// every byte sequence that is not, a byte order mark starting it dropped, scripting enabled (so
/// `<noscript>` is raw text, as the injector takes it).
pub fn parse(document: &[u8]) -> Tree {
    let (decoded, origin) = oracle::decode(document);
    let decoded = String::from_utf8(decoded).expect("decoded to UTF-8");
    let sink = Sink::default();
    let offset = sink.offset.clone();
    let mut options = ParseOpts::default();
    // html5ever drops a byte order mark starting any input it is fed, not just the document's:
    // only that one goes, as the decoder drops it.
    options.tokenizer.discard_bom = false;
    let mut parser = parse_document(sink, options);
    let mut end = 0;
    let bom = '\u{feff}';
    if decoded.starts_with(bom) {
        end = bom.len_utf8();
    }
    for piece in decoded[end..].split_inclusive('>') {
        end += piece.len();
        offset.set(origin[end]);
        parser.process(StrTendril::from_slice(piece));
    }
    offset.set(document.len());
    return parser.finish();
}

/// Builds a [`Tree`], see [`parse`].
struct Sink {
    document: Handle,
    count: Cell<usize>,
    /// Where the input stands, shared with [`parse`].
    offset: Rc<Cell<usize>>,
}

impl Default for Sink {
    fn default() -> Self {
        let offset = Rc::new(Cell::new(0));
        return Self {
            document: new_node(0, 0, Kind::Document),
            count: Cell::new(1),
            offset,
        };
    }
}

fn new_node(id: usize, offset: usize, kind: Kind) -> Handle {
    return Rc::new(Node {
        id,
        offset,
        kind,
        attributes: RefCell::default(),
        text: RefCell::default(),
        parent: RefCell::default(),
        children: RefCell::default(),
        popped: Cell::default(),
        popped_at: Cell::default(),
    });
}

impl Sink {
    fn node(&self, kind: Kind) -> Handle {
        let id = self.count.get();
        self.count.set(id + 1);
        return new_node(id, self.offset.get(), kind);
    }

    fn text_node(&self, text: &str) -> Handle {
        let node = self.node(Kind::Text);
        node.text.borrow_mut().push_str(text);
        return node;
    }

    fn detach(&self, node: &Handle) {
        if let Some(parent) = node
            .parent
            .borrow_mut()
            .take()
            .and_then(|p| return p.upgrade())
        {
            parent
                .children
                .borrow_mut()
                .retain(|child| return !Rc::ptr_eq(child, node));
        }
    }

    fn insert(&self, parent: &Handle, index: usize, child: Handle) {
        *child.parent.borrow_mut() = Some(Rc::downgrade(parent));
        parent.children.borrow_mut().insert(index, child);
    }
}

impl TreeSink for Sink {
    type Handle = Handle;
    type Output = Tree;
    type ElemName<'a> = &'a QualName;

    fn finish(self) -> Tree {
        return Tree {
            document: self.document,
            count: self.count.get(),
        };
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> Handle {
        return self.document.clone();
    }

    fn elem_name<'a>(&'a self, target: &'a Handle) -> &'a QualName {
        return target.name().expect("an element");
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Handle {
        let template_contents = flags.template.then(|| return self.node(Kind::Fragment));
        let node = self.node(Kind::Element {
            name,
            template_contents,
        });
        *node.attributes.borrow_mut() = attrs;
        return node;
    }

    fn create_comment(&self, text: StrTendril) -> Handle {
        let node = self.node(Kind::Comment);
        node.text.borrow_mut().push_str(&text);
        return node;
    }

    fn create_pi(&self, _target: StrTendril, data: StrTendril) -> Handle {
        // Only XML has processing instructions: HTML reads them as comments.
        return self.create_comment(data);
    }

    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        let index = parent.children.borrow().len();
        match child {
            NodeOrText::AppendNode(node) => self.insert(parent, index, node),
            NodeOrText::AppendText(text) => {
                let last = parent.children.borrow().last().cloned();
                match last.filter(|last| return matches!(last.kind, Kind::Text)) {
                    Some(last) => last.text.borrow_mut().push_str(&text),
                    None => self.insert(parent, index, self.text_node(&text)),
                }
            }
        }
    }

    fn append_based_on_parent_node(
        &self,
        element: &Handle,
        prev_element: &Handle,
        child: NodeOrText<Handle>,
    ) {
        match element.parent().is_some() {
            true => self.append_before_sibling(element, child),
            false => self.append(prev_element, child),
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let node = self.node(Kind::Doctype {
            name: String::from(&*name),
            public_id: String::from(&*public_id),
            system_id: String::from(&*system_id),
        });
        self.append(&self.document, NodeOrText::AppendNode(node));
    }

    fn pop(&self, node: &Handle) {
        if node.popped.get().is_none() {
            node.popped.set(Some(self.count.get()));
            node.popped_at.set(Some(self.offset.get()));
        }
    }

    fn get_template_contents(&self, target: &Handle) -> Handle {
        return match &target.kind {
            Kind::Element {
                template_contents: Some(contents),
                ..
            } => contents.clone(),
            _ => panic!("not a template"),
        };
    }

    fn same_node(&self, x: &Handle, y: &Handle) -> bool {
        return Rc::ptr_eq(x, y);
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    fn append_before_sibling(&self, sibling: &Handle, new_node: NodeOrText<Handle>) {
        let parent = sibling.parent().expect("a sibling has a parent");
        let index = parent
            .children
            .borrow()
            .iter()
            .position(|child| return Rc::ptr_eq(child, sibling))
            .expect("a child of its parent");
        let previous = index
            .checked_sub(1)
            .map(|at| return parent.children.borrow()[at].clone())
            .filter(|previous| return matches!(previous.kind, Kind::Text));
        match new_node {
            NodeOrText::AppendText(text) => match previous {
                Some(previous) => previous.text.borrow_mut().push_str(&text),
                None => self.insert(&parent, index, self.text_node(&text)),
            },
            NodeOrText::AppendNode(node) => {
                self.detach(&node);
                // Detaching may have moved the sibling.
                let index = parent
                    .children
                    .borrow()
                    .iter()
                    .position(|child| return Rc::ptr_eq(child, sibling))
                    .expect("a child of its parent");
                self.insert(&parent, index, node);
            }
        }
    }

    fn add_attrs_if_missing(&self, target: &Handle, attrs: Vec<Attribute>) {
        let mut existing = target.attributes.borrow_mut();
        for attribute in attrs {
            if !existing
                .iter()
                .any(|known| return known.name == attribute.name)
            {
                existing.push(attribute);
            }
        }
    }

    fn remove_from_parent(&self, target: &Handle) {
        self.detach(target);
    }

    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        let children = std::mem::take(&mut *node.children.borrow_mut());
        for child in children {
            *child.parent.borrow_mut() = None;
            let last = new_parent.children.borrow().last().cloned();
            match (last, matches!(child.kind, Kind::Text)) {
                (Some(last), true) if matches!(last.kind, Kind::Text) => {
                    last.text.borrow_mut().push_str(&child.text.borrow());
                }
                _ => {
                    let index = new_parent.children.borrow().len();
                    self.insert(new_parent, index, child);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(document: &str) -> String {
        return parse(document.as_bytes()).serialize(None);
    }

    /// Cases after html5lib-tests' tree construction tests, which html5ever passes: the sink builds
    /// what the parser asks it to.
    #[test]
    fn html5lib_cases() {
        let cases: &[(&str, &str)] = &[
            (
                "<!DOCTYPE html>Test",
                "| <!DOCTYPE html>\n| <html>\n|   <head>\n|   <body>\n|     \"Test\"\n",
            ),
            (
                "<p>One<p>Two",
                "| <html>\n|   <head>\n|   <body>\n|     <p>\n|       \"One\"\n|     <p>\n|       \"Two\"\n",
            ),
            (
                "<template><p>x</p></template>",
                "| <html>\n|   <head>\n|     <template>\n|       content\n|         <p>\n|           \"x\"\n|   <body>\n",
            ),
            (
                "<svg xlink:href=a viewBox='0 0 1 1'></svg>",
                "| <html>\n|   <head>\n|   <body>\n|     <svg svg>\n|       viewBox=\"0 0 1 1\"\n|       xlink href=\"a\"\n",
            ),
            // Foster parenting, before the table.
            (
                "<table>a<tr>",
                "| <html>\n|   <head>\n|   <body>\n|     \"a\"\n|     <table>\n|       <tbody>\n|         <tr>\n",
            ),
            // The adoption agency, which reparents children.
            (
                "<b><p>x</b>y",
                "| <html>\n|   <head>\n|   <body>\n|     <b>\n|     <p>\n|       <b>\n|         \"x\"\n|       \"y\"\n",
            ),
            (
                "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \"x\"><!--c-->",
                "| <!DOCTYPE html \"-//W3C//DTD HTML 4.01//EN\" \"x\">\n| <!-- c -->\n| <html>\n|   <head>\n|   <body>\n",
            ),
        ];
        for (document, expected) in cases {
            assert_eq!(&tree(document), expected, "{document}");
        }
    }

    /// The constructs behind lol_html's exceptions, which the tree builder settles.
    #[test]
    fn oracle_exception_cases() {
        let cases: &[(&str, &str)] = &[
            // An implied head, which `</head>` closes.
            (
                "<html><meta charset=utf-8><title>t</title></head>",
                "| <html>\n|   <head>\n|     <meta>\n|       charset=\"utf-8\"\n|     <title>\n|       \"t\"\n|   <body>\n",
            ),
            // `</p>` within a template is ignored, the template holding on.
            (
                "<head><template></p><meta name=a></template><meta charset=utf-8>",
                "| <html>\n|   <head>\n|     <template>\n|       content\n|         <meta>\n|           name=\"a\"\n|     <meta>\n|       charset=\"utf-8\"\n|   <body>\n",
            ),
            // An `<html amp>` tag past the head merges into the root.
            (
                "<html><head><html amp>",
                "| <html>\n|   amp=\"\"\n|   <head>\n|   <body>\n",
            ),
            // An empty unquoted value ends at the `>`.
            (
                "<head><script/c=>x</script>",
                "| <html>\n|   <head>\n|     <script>\n|       c=\"\"\n|       \"x\"\n|   <body>\n",
            ),
            // A byte order mark is part of an attribute's name, and of a doctype's.
            (
                "<!DOCTYPE \u{feff}html><meta \u{feff}charset=utf-8>",
                "| <!DOCTYPE \u{feff}html>\n| <html>\n|   <head>\n|     <meta>\n|       \u{feff}charset=\"utf-8\"\n|   <body>\n",
            ),
            // One starting the document is dropped, not one past a tag.
            ("\u{feff}<head>", "| <html>\n|   <head>\n|   <body>\n"),
            (
                "<p>\u{feff}x",
                "| <html>\n|   <head>\n|   <body>\n|     <p>\n|       \"\u{feff}x\"\n",
            ),
            // `&#32` is a space, the `#32` after it text closing the head.
            (
                "<head>&#32#32<meta>",
                "| <html>\n|   <head>\n|     \" \"\n|   <body>\n|     \"#32\"\n|     <meta>\n",
            ),
            // Noscript is raw text, scripting being enabled.
            (
                "<head><noscript><meta charset=utf-8></noscript>",
                "| <html>\n|   <head>\n|     <noscript>\n|       \"<meta charset=utf-8>\"\n|   <body>\n",
            ),
        ];
        for (document, expected) in cases {
            assert_eq!(&tree(document), expected, "{document}");
        }
    }

    #[test]
    fn positions() {
        let parsed = parse(b"<html><head><meta name=a>\xff<p>");
        let head = parsed.head().unwrap();
        assert_eq!(head.offset, 12);
        assert_eq!(head.children()[0].offset, 25);
        // The bytes that are not UTF-8 count as they are.
        let body = parsed.root().unwrap().children()[1].clone();
        assert!(body.is_html("body"));
        assert_eq!(body.children()[1].offset, 29);

        // What the parser puts in the head once it closed it comes after.
        let parsed = parse(b"<head></head><meta name=a>");
        let head = parsed.head().unwrap();
        let meta = head.children()[0].clone();
        assert!(head.popped.get().unwrap() <= meta.id);
    }

    #[test]
    fn serialize_skips_and_merges() {
        let parsed = parse(b"<head> <script data-dd-probe></script> <meta name=a>");
        let probe = parsed.elements_with("data-dd-probe").pop().unwrap();
        assert_eq!(
            parsed.serialize_significant(Some(&probe)),
            parse(b"<head>  <meta name=a>").serialize_significant(None)
        );
        assert!(parsed.serialize(Some(&probe)).contains("\"  \""));
    }
}
