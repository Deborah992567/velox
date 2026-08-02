//! The configuration AST.
//!
//! A parsed configuration is a tree of [`ConfigNode`]s. Each node is either a
//! leaf directive (`worker_processes 4;`) or a block
//! (`http { server { ... } }`). All argument values are raw strings at this
//! stage; typed conversion happens during validation.

use crate::config::Pos;
use crate::core::SourcePos;

/// A single directive (leaf) or block in the configuration tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigNode {
    /// The directive name (`worker_processes`, `server`, ...).
    pub name: String,
    /// The arguments of the directive, in order. Empty for `server {}`-style
    /// blocks with no arguments.
    pub args: Vec<String>,
    /// Child directives for block directives; empty for leaf directives.
    pub children: Vec<Self>,
    /// Where the directive name starts.
    pub pos: Pos,
}

impl ConfigNode {
    /// Create a leaf directive with no children.
    #[must_use]
    pub fn leaf(name: impl Into<String>, args: Vec<String>, pos: Pos) -> Self {
        Self {
            name: name.into(),
            args,
            children: Vec::new(),
            pos,
        }
    }

    /// Create a block directive with children.
    #[must_use]
    pub fn block(
        name: impl Into<String>,
        args: Vec<String>,
        children: Vec<Self>,
        pos: Pos,
    ) -> Self {
        Self {
            name: name.into(),
            args,
            children,
            pos,
        }
    }

    /// Whether this is a block directive (has children).
    #[must_use]
    pub const fn is_block(&self) -> bool {
        !self.children.is_empty()
    }

    /// A source position usable in [`crate::core::Error`] diagnostics.
    #[must_use]
    pub fn source_pos(&self, file: &str) -> SourcePos {
        SourcePos {
            file: file.to_owned(),
            line: self.pos.line,
            column: self.pos.column,
        }
    }
}

/// A parsed configuration file: the sequence of top-level directives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRoot {
    /// Top-level directives.
    pub nodes: Vec<ConfigNode>,
}

impl ConfigRoot {
    /// Iterate over all nodes in declaration order (depth-first), including
    /// the root nodes themselves.
    pub fn walk(&self) -> impl Iterator<Item = &ConfigNode> {
        Walk::new(&self.nodes)
    }
}

/// Depth-first iterator over a directive tree.
struct Walk<'a> {
    stack: Vec<std::slice::Iter<'a, ConfigNode>>,
}

impl<'a> Walk<'a> {
    fn new(nodes: &'a [ConfigNode]) -> Self {
        Self {
            stack: vec![nodes.iter()],
        }
    }
}

impl<'a> Iterator for Walk<'a> {
    type Item = &'a ConfigNode;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let Some(node) = self.stack.last_mut()?.next() else {
                self.stack.pop();
                if self.stack.is_empty() {
                    return None;
                }
                continue;
            };
            if !node.children.is_empty() {
                self.stack.push(node.children.iter());
            }
            return Some(node);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Pos;
    use crate::config::ast::{ConfigNode, ConfigRoot};

    fn pos(line: usize, column: usize) -> Pos {
        Pos { line, column }
    }

    #[test]
    fn walk_visits_depth_first_in_declaration_order() {
        let http = ConfigNode::block(
            "http",
            Vec::new(),
            vec![
                ConfigNode::leaf("server", vec!["80".into()], pos(2, 1)),
                ConfigNode::block(
                    "upstream",
                    vec!["backend".into()],
                    vec![ConfigNode::leaf(
                        "server",
                        vec!["127.0.0.1:8080".into()],
                        pos(4, 3),
                    )],
                    pos(3, 1),
                ),
                ConfigNode::leaf("gzip", vec!["on".into()], pos(5, 1)),
            ],
            pos(1, 1),
        );
        let root = ConfigRoot {
            nodes: vec![
                ConfigNode::leaf("worker_processes", vec!["4".into()], pos(0, 0)),
                http,
            ],
        };

        let names: Vec<_> = root.walk().map(|node| node.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "worker_processes",
                "http",
                "server",
                "upstream",
                "server",
                "gzip",
            ]
        );
    }
}
