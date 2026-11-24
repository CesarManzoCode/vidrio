//! CSS Syntax 3 and CSSOM without a JavaScript realm: tokenization of
//! edge cases (escapes, numbers, `url(`), error recovery of the parser,
//! serialization of rules and of shorthands in `cssText`, and
//! `insertRule`/`deleteRule` with their hierarchy errors.

use vidrio::css::declaration::DeclarationBlock;
use vidrio::css::om::{
    self, Owner, RuleError, RuleKind, SheetRef, constructed_sheet, delete_rule, insert_rule,
    parse_sheet, replace_sync, serialize_rule,
};
use vidrio::css::parser::{BlockItem, RawRule, parse_rule, parse_stylesheet};
use vidrio::css::tokenizer::{Numeric, Token, tokenize};

fn texts(sheet: &SheetRef) -> Vec<String> {
    sheet
        .borrow()
        .rules
        .iter()
        .map(|r| serialize_rule(&r.borrow()))
        .collect()
}

fn num(value: f64, integer: bool, signed: bool) -> Numeric {
    Numeric {
        value,
        integer,
        signed,
    }
}

// ----- tokenizer -------------------------------------------------------------------

#[test]
fn escapes_in_identifiers() {
    assert_eq!(tokenize("\\41 b"), vec![Token::Ident("Ab".into())]);
    assert_eq!(tokenize("\\0"), vec![Token::Ident("\u{FFFD}".into())]);
    assert_eq!(tokenize("\\110000"), vec![Token::Ident("\u{FFFD}".into())]);
    assert_eq!(tokenize("a\\ b"), vec![Token::Ident("a b".into())]);
    // A backslash before a newline is not an escape.
    assert_eq!(
        tokenize("\\\n"),
        vec![Token::Delim('\\'), Token::Whitespace]
    );
    // An escape at EOF is U+FFFD.
    assert_eq!(tokenize("a\\"), vec![Token::Ident("a\u{FFFD}".into())]);
}

#[test]
fn numbers() {
    assert_eq!(tokenize("12"), vec![Token::Number(num(12.0, true, false))]);
    assert_eq!(tokenize("+.5"), vec![Token::Number(num(0.5, false, true))]);
    assert_eq!(
        tokenize("-1e3"),
        vec![Token::Number(num(-1000.0, false, true))]
    );
    assert_eq!(
        tokenize("1e"),
        vec![Token::Dimension(num(1.0, true, false), "e".into())]
    );
    assert_eq!(
        tokenize("10%"),
        vec![Token::Percentage(num(10.0, true, false))]
    );
    assert_eq!(
        tokenize("3px"),
        vec![Token::Dimension(num(3.0, true, false), "px".into())]
    );
    // `1.` is a number followed by a delimiter.
    assert_eq!(
        tokenize("1."),
        vec![Token::Number(num(1.0, true, false)), Token::Delim('.')]
    );
}

#[test]
fn urls() {
    assert_eq!(tokenize("url(a.png)"), vec![Token::Url("a.png".into())]);
    assert_eq!(tokenize("url(  a.png  )"), vec![Token::Url("a.png".into())]);
    assert_eq!(tokenize("URL(\\)x)"), vec![Token::Url(")x".into())]);
    // A quoted argument makes `url(` a function.
    assert_eq!(
        tokenize("url( 'a' )"),
        vec![
            Token::Function("url".into()),
            Token::Whitespace,
            Token::String("a".into()),
            Token::Whitespace,
            Token::RightParen,
        ]
    );
    assert_eq!(tokenize("url(a b)"), vec![Token::BadUrl]);
    assert_eq!(tokenize("url(a\"b)"), vec![Token::BadUrl]);
    // An unclosed url at EOF still is a url.
    assert_eq!(tokenize("url(a"), vec![Token::Url("a".into())]);
}

#[test]
fn strings_and_comments() {
    assert_eq!(tokenize("'a\\\nb'"), vec![Token::String("ab".into())]);
    assert_eq!(
        tokenize("'a\nb'"),
        vec![
            Token::BadString,
            Token::Whitespace,
            Token::Ident("b".into()),
            Token::String(String::new()),
        ]
    );
    assert_eq!(
        tokenize("/* x */a/* unclosed"),
        vec![Token::Ident("a".into())]
    );
    assert_eq!(
        tokenize("<!-- -->"),
        vec![Token::Cdo, Token::Whitespace, Token::Cdc]
    );
}

// ----- parser ----------------------------------------------------------------------

#[test]
fn parser_recovers_from_errors() {
    let rules = parse_stylesheet("a { color: red; ;; 12px; width: 1px } } b {} @x y; c { }");
    // The stray `}` and the unknown at-rule do not stop the parser.
    assert!(rules.len() >= 3);
    let RawRule::Qualified(a) = &rules[0] else {
        panic!("a qualified rule first");
    };
    let BlockItem::Declarations(decls) = &a.block[0] else {
        panic!("declarations");
    };
    assert_eq!(decls.len(), 2);
    assert_eq!(decls[1].name, "width");
    // An unclosed block ends at EOF.
    let rules = parse_stylesheet("a { color: red");
    assert_eq!(rules.len(), 1);
    // "Parse a rule" wants exactly one rule.
    assert!(parse_rule("a {} b {}", false).is_err());
    assert!(parse_rule("", false).is_err());
    assert!(parse_rule("  @media all {}  ", false).is_ok());
}

#[test]
fn invalid_rules_are_dropped() {
    let s =
        parse_sheet("a!b { color: red } c { color: blue } @media { d {} } @supports (x) { e {} }");
    assert_eq!(
        texts(&s),
        vec![
            "c { color: blue; }",
            "@media {\n  d { }\n}",
            "@supports (x) {\n  e { }\n}",
        ]
    );
    // `(x)` is `<general-enclosed>`: valid, and false.
    assert!(matches!(
        s.borrow().rules[2].borrow().kind,
        RuleKind::Supports { matches: false, .. }
    ));
    // A bad declaration only drops itself.
    let s = parse_sheet("a { color: ; width: 1px; height: 2px !imp }");
    assert_eq!(texts(&s), vec!["a { width: 1px; }"]);
}

#[test]
fn nested_rules() {
    let s = parse_sheet(".a { color: red; &:hover { color: blue } .b & { x: y } top: 0 }");
    assert_eq!(
        texts(&s),
        vec![".a {\n  color: red;\n  &:hover { color: blue; }\n  .b & { }\n  top: 0px;\n}"]
    );
    let rule = s.borrow().rules[0].clone();
    let b = rule.borrow();
    let children = b.kind.children().expect("children");
    assert!(matches!(
        children[2].borrow().kind,
        RuleKind::NestedDeclarations { .. }
    ));
}

// ----- serialization ---------------------------------------------------------------

#[test]
fn rule_serialization() {
    let s = parse_sheet(concat!(
        "@import url(\"a.css\") supports(display: grid) screen and (min-width: 100px);",
        "@namespace svg url(http://www.w3.org/2000/svg);",
        "svg|a  >  b,c~d{margin:0 0 0 0}",
        "@media screen{a{color:red}}",
        "@font-face{font-family:x;src:url(x.woff)}",
        "@page :first{margin:1in}",
        "@layer a.b,c;",
        "@property --x{syntax:'<length>';inherits:false;initial-value:0px}",
        "@scope (.a) to (.b){c{}}",
    ));
    let t = texts(&s);
    assert_eq!(
        t[0],
        "@import url(\"a.css\") supports(display: grid) screen and (min-width: 100px);"
    );
    assert_eq!(t[1], "@namespace svg url(\"http://www.w3.org/2000/svg\");");
    assert_eq!(t[2], "svg|a > b, c ~ d { margin: 0px; }");
    assert_eq!(t[3], "@media screen {\n  a { color: red; }\n}");
    assert!(t[4].starts_with("@font-face { font-family: x; src: url("));
    assert_eq!(t[5], "@page :first { margin: 1in; }");
    assert_eq!(t[6], "@layer a.b, c;");
    assert_eq!(
        t[7],
        "@property --x { syntax: \"<length>\"; inherits: false; initial-value: 0px; }"
    );
    assert_eq!(t[8], "@scope (.a) to (.b) {\n  c { }\n}");
}

#[test]
fn shorthands_in_css_text() {
    let mut b = DeclarationBlock::parse(
        "margin-top: 1px; margin-right: 2px; margin-bottom: 1px; margin-left: 2px",
    );
    assert_eq!(b.serialize(), "margin: 1px 2px;");
    assert_eq!(b.property_value("margin"), "1px 2px");
    b.set_property("margin-left", "3px", "important");
    // Mixed importance: no shorthand.
    assert_eq!(b.property_value("margin"), "");
    assert_eq!(
        b.serialize(),
        "margin-top: 1px; margin-right: 2px; margin-bottom: 1px; margin-left: 3px !important;"
    );
    let b = DeclarationBlock::parse("border: 1px solid red");
    assert_eq!(b.serialize(), "border: 1px solid red;");
    let b = DeclarationBlock::parse("padding: 1px; padding-top: 2px");
    assert_eq!(b.serialize(), "padding: 2px 1px 1px;");
    let b = DeclarationBlock::parse("margin: inherit");
    assert_eq!(b.serialize(), "margin: inherit;");
}

#[test]
fn css_escape_and_supports() {
    assert_eq!(om::css_escape("0a"), "\\30 a");
    assert_eq!(om::css_escape("-"), "\\-");
    assert_eq!(om::css_escape("a b"), "a\\ b");
    assert_eq!(om::css_escape("\0"), "\u{FFFD}");
    assert!(om::css_supports_property("color", "red"));
    assert!(!om::css_supports_property("color", "red !important"));
    assert!(om::css_supports_property("--x", "anything"));
    assert!(om::css_supports_condition("(color: red) or (bogus: 1)"));
    assert!(om::css_supports_condition("selector(:is(a, b))"));
    assert!(!om::css_supports_condition("(color: red) and (bogus: 1)"));
}

// ----- insertRule and deleteRule ---------------------------------------------------

#[test]
fn insert_rule_errors() {
    let s = parse_sheet("@import 'a.css'; a {}");
    let o = Owner::Sheet(s.clone());
    assert_eq!(insert_rule(&o, "b {}", 3), Err(RuleError::IndexSize));
    assert_eq!(insert_rule(&o, "b {", 2), Ok(2));
    assert_eq!(insert_rule(&o, "b {} c {}", 0), Err(RuleError::Syntax));
    assert_eq!(insert_rule(&o, "", 0), Err(RuleError::Syntax));
    // `@import` after a style rule, a style rule before `@import`.
    assert_eq!(
        insert_rule(&o, "@import 'b.css';", 2),
        Err(RuleError::HierarchyRequest)
    );
    assert_eq!(insert_rule(&o, "x {}", 0), Err(RuleError::HierarchyRequest));
    assert_eq!(insert_rule(&o, "@import 'b.css';", 1), Ok(1));
    // `@namespace` in a sheet with style rules.
    assert_eq!(
        insert_rule(&o, "@namespace x url(y);", 2),
        Err(RuleError::InvalidState)
    );
    assert_eq!(s.borrow().rules.len(), 4);
}

#[test]
fn grouping_rules_reject_imports_and_namespaces() {
    let s = parse_sheet("@media all { a {} } b { c {} }");
    let media = Owner::Rule(s.borrow().rules[0].clone());
    assert_eq!(
        insert_rule(&media, "@import 'x.css';", 0),
        Err(RuleError::HierarchyRequest)
    );
    assert_eq!(
        insert_rule(&media, "@namespace url(x);", 0),
        Err(RuleError::HierarchyRequest)
    );
    assert_eq!(insert_rule(&media, "d {}", 1), Ok(1));
    assert_eq!(delete_rule(&media, 2), Err(RuleError::IndexSize));
    assert_eq!(delete_rule(&media, 0), Ok(()));
    // A style rule takes nested rules and bare declarations.
    let style = Owner::Rule(s.borrow().rules[1].clone());
    assert_eq!(insert_rule(&style, "> e {}", 0), Ok(0));
    assert_eq!(insert_rule(&style, "color: red", 0), Ok(0));
    let first = style.item(0).expect("a rule");
    assert!(matches!(
        first.borrow().kind,
        RuleKind::NestedDeclarations { .. }
    ));
    assert_eq!(serialize_rule(&first.borrow()), "color: red;");
}

#[test]
fn delete_rule_and_namespaces() {
    let s = parse_sheet("@namespace url(x); a {}");
    let o = Owner::Sheet(s.clone());
    assert_eq!(delete_rule(&o, 0), Err(RuleError::InvalidState));
    assert_eq!(delete_rule(&o, 1), Ok(()));
    assert_eq!(delete_rule(&o, 0), Ok(()));
    assert!(o.is_empty());
    assert_eq!(delete_rule(&o, 0), Err(RuleError::IndexSize));
}

#[test]
fn constructed_sheets_drop_imports() {
    let s = constructed_sheet(None, "screen", false);
    assert_eq!(
        replace_sync(&s, "@import 'a.css'; a { color: red }"),
        Ok(())
    );
    assert_eq!(texts(&s), vec!["a { color: red; }"]);
    let o = Owner::Sheet(s.clone());
    assert_eq!(
        insert_rule(&o, "@import 'a.css';", 0),
        Err(RuleError::Syntax)
    );
    s.borrow_mut().disallow_modification = true;
    assert_eq!(insert_rule(&o, "b {}", 0), Err(RuleError::NotAllowed));
    assert_eq!(replace_sync(&s, ""), Err(RuleError::NotAllowed));
    // A sheet that is not constructed cannot be replaced.
    let inline = parse_sheet("");
    assert_eq!(replace_sync(&inline, ""), Err(RuleError::NotAllowed));
}
