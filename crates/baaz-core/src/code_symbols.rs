//! Symbol definitions for `code_find` (docs/85 12.6): Rust, TypeScript and
//! JavaScript, and Python through tree-sitter (the grammars gpui-kit
//! already builds); Go through a small scanner of its declarations
//! (`func`, `type`), since its grammar is not in the build. Every other
//! file is text search only.

use tree_sitter::{Language, Node, Parser};

/// One definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// The name.
    pub name: String,
    /// `fn`, `struct`, `class`, …
    pub kind: String,
    /// 1-based line.
    pub line: usize,
}

/// The node kinds that define something, per grammar, with the word the
/// result names them by.
fn definers(language: &str) -> &'static [(&'static str, &'static str)] {
    match language {
        "rust" => &[
            ("function_item", "fn"),
            ("function_signature_item", "fn"),
            ("struct_item", "struct"),
            ("enum_item", "enum"),
            ("trait_item", "trait"),
            ("type_item", "type"),
            ("const_item", "const"),
            ("static_item", "static"),
            ("mod_item", "mod"),
            ("macro_definition", "macro"),
        ],
        "python" => &[("function_definition", "def"), ("class_definition", "class")],
        _ => &[
            ("function_declaration", "function"),
            ("generator_function_declaration", "function"),
            ("class_declaration", "class"),
            ("abstract_class_declaration", "class"),
            ("interface_declaration", "interface"),
            ("type_alias_declaration", "type"),
            ("enum_declaration", "enum"),
            ("method_definition", "method"),
            ("variable_declarator", "const"),
        ],
    }
}

fn grammar(path: &std::path::Path) -> Option<(Language, &'static str)> {
    let ext = path.extension()?.to_str()?;
    Some(match ext {
        "rs" => (tree_sitter_rust::LANGUAGE.into(), "rust"),
        "py" => (tree_sitter_python::LANGUAGE.into(), "python"),
        "ts" | "mts" | "cts" => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), "typescript"),
        "tsx" | "js" | "jsx" | "mjs" | "cjs" => (tree_sitter_typescript::LANGUAGE_TSX.into(), "typescript"),
        _ => return None,
    })
}

/// The definitions in `text`, the file at `path`.
pub fn definitions(path: &std::path::Path, text: &str) -> Vec<Symbol> {
    if path.extension().is_some_and(|ext| ext == "go") {
        return go_definitions(text);
    }
    let Some((language, name)) = grammar(path) else { return Vec::new() };
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else { return Vec::new() };
    let mut out = Vec::new();
    walk(tree.root_node(), text.as_bytes(), definers(name), &mut out);
    out
}

fn walk(node: Node, source: &[u8], definers: &[(&str, &str)], out: &mut Vec<Symbol>) {
    if let Some((_, kind)) = definers.iter().find(|(node_kind, _)| *node_kind == node.kind()) {
        // A JS/TS `const x = …` names a definition only when it holds a
        // function or a class.
        let wanted = node.kind() != "variable_declarator"
            || node.child_by_field_name("value").is_some_and(|value| {
                matches!(value.kind(), "arrow_function" | "function" | "function_expression" | "class")
            });
        if let Some(name) = node.child_by_field_name("name").filter(|_| wanted) {
            if let Ok(text) = name.utf8_text(source) {
                out.push(Symbol { name: text.to_owned(), kind: (*kind).to_owned(), line: node.start_position().row + 1 });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, definers, out);
    }
}

/// Go's `func Name`, `func (r T) Name` and `type Name` lines.
fn go_definitions(text: &str) -> Vec<Symbol> {
    let ident = |rest: &str| rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<String>();
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let (kind, rest) = if let Some(rest) = line.strip_prefix("func ") {
                let rest = if rest.starts_with('(') { rest.split_once(')').map(|(_, after)| after.trim_start())? } else { rest };
                ("func", rest)
            } else {
                ("type", line.strip_prefix("type ")?)
            };
            let name = ident(rest);
            (!name.is_empty()).then(|| Symbol { name, kind: kind.to_owned(), line: index + 1 })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn names(path: &str, text: &str) -> Vec<(String, String, usize)> {
        definitions(Path::new(path), text).into_iter().map(|s| (s.name, s.kind, s.line)).collect()
    }

    #[test]
    fn each_language_names_its_definitions() {
        assert_eq!(
            names("a.rs", "pub struct Range;\nimpl Range {\n    pub fn parse(s: &str) -> Self { Range }\n}\nmacro_rules! m { () => {} }\n"),
            [("Range".into(), "struct".into(), 1), ("parse".into(), "fn".into(), 3), ("m".into(), "macro".into(), 5)]
        );
        assert_eq!(
            names("a.py", "class Checker:\n    def check(self):\n        pass\n"),
            [("Checker".into(), "class".into(), 1), ("check".into(), "def".into(), 2)]
        );
        assert_eq!(
            names("a.ts", "export interface Props {}\nexport function render() {}\nconst handler = () => 1;\nconst count = 3;\nclass View { draw() {} }\n"),
            [
                ("Props".into(), "interface".into(), 1),
                ("render".into(), "function".into(), 2),
                ("handler".into(), "const".into(), 3),
                ("View".into(), "class".into(), 5),
                ("draw".into(), "method".into(), 5),
            ]
        );
        assert_eq!(names("a.jsx", "function App() { return <div/>; }\n")[0].0, "App");
        assert_eq!(
            names("a.go", "package x\n\ntype Server struct{}\n\nfunc (s *Server) Serve() {}\nfunc main() {}\n"),
            [("Server".into(), "type".into(), 3), ("Serve".into(), "func".into(), 5), ("main".into(), "func".into(), 6)]
        );
        assert!(names("notes.md", "# Range").is_empty(), "other files are text only");
    }
}
