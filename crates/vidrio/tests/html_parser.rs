//! Hand-written html5lib-format cases for the HTML parser (adoption
//! agency, foster parenting, `template`, foreign content, quirks and
//! fragments), compared on the html5lib tree dump, plus fragment
//! serialization cases.

use vidrio::dom::{Atom, Document, Namespace, NodeId, QualName, QuirksMode};
use vidrio::html::{html5lib_tree, parse_document, parse_fragment, parse_into, serialize_fragment};

/// One test of the html5lib tree-construction format.
struct Case {
    data: String,
    fragment: Option<String>,
    scripting: bool,
    document: String,
}

/// Parses `#data` / `#errors` / `#document-fragment` / `#script-on` /
/// `#script-off` / `#document` sections; a blank line ends a test.
fn cases(text: &str) -> Vec<Case> {
    let mut out = Vec::new();
    let mut section = "";
    let mut data: Vec<&str> = Vec::new();
    let mut document: Vec<&str> = Vec::new();
    let mut fragment = None;
    let mut scripting = true;
    let mut finish = |data: &mut Vec<&str>,
                      document: &mut Vec<&str>,
                      fragment: &mut Option<String>,
                      scripting: &mut bool| {
        if !document.is_empty() {
            out.push(Case {
                data: data.join("\n"),
                fragment: fragment.take(),
                scripting: *scripting,
                document: document.join("\n"),
            });
        }
        data.clear();
        document.clear();
        *fragment = None;
        *scripting = true;
    };
    for line in text.lines() {
        if line.starts_with('#') {
            section = line;
            match line {
                "#data" => finish(&mut data, &mut document, &mut fragment, &mut scripting),
                "#script-on" => scripting = true,
                "#script-off" => scripting = false,
                _ => {}
            }
            continue;
        }
        match section {
            "#data" => data.push(line),
            "#document-fragment" if !line.is_empty() => fragment = Some(line.to_string()),
            "#document" if !line.is_empty() => document.push(line),
            _ => {}
        }
    }
    finish(&mut data, &mut document, &mut fragment, &mut scripting);
    out
}

/// The fragment context element named like html5lib does (`td`,
/// `svg path`, `math mi`).
fn context_element(d: &mut Document, name: &str) -> NodeId {
    let (ns, local) = match name.split_once(' ') {
        Some(("svg", local)) => (Namespace::Svg, local),
        Some(("math", local)) => (Namespace::MathMl, local),
        _ => (Namespace::Html, name),
    };
    d.create_element(QualName::new(Some(ns), None, Atom::new(local)))
}

fn run(text: &str) {
    let mut failures = Vec::new();
    let all = cases(text);
    assert!(!all.is_empty(), "no cases parsed");
    for case in &all {
        let got = match &case.fragment {
            Some(context) => {
                let mut d = Document::new_html();
                let context = context_element(&mut d, context);
                let fragment = parse_fragment(&mut d, context, &case.data, case.scripting);
                html5lib_tree(&d, fragment)
            }
            None => {
                let d = parse_document(&case.data, case.scripting);
                html5lib_tree(&d, NodeId::DOCUMENT)
            }
        };
        if got != case.document {
            failures.push(format!(
                "#data\n{}\n#expected\n{}\n#got\n{}\n",
                case.data, case.document, got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}

#[test]
fn adoption_agency() {
    run(r#"#data
<p>1<b>2<i>3</b>4</i>5</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "1"
|       <b>
|         "2"
|         <i>
|           "3"
|       <i>
|         "4"
|       "5"

#data
<b>1<p>2</b>3</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <b>
|       "1"
|     <p>
|       <b>
|         "2"
|       "3"

#data
<a><p>X<a>Y</a>Z</p></a>
#errors
#document
| <html>
|   <head>
|   <body>
|     <a>
|     <p>
|       <a>
|         "X"
|       <a>
|         "Y"
|       "Z"

#data
<a href=a>1<a href=b>2</a>3
#errors
#document
| <html>
|   <head>
|   <body>
|     <a>
|       href="a"
|       "1"
|     <a>
|       href="b"
|       "2"
|     "3"

#data
<b><i><p>x</b>y
#errors
#document
| <html>
|   <head>
|   <body>
|     <b>
|       <i>
|     <i>
|       <p>
|         <b>
|           "x"
|         "y"

#data
<p><b><i><u></p> <p>X
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       <b>
|         <i>
|           <u>
|     <b>
|       <i>
|         <u>
|           " "
|           <p>
|             "X"
"#);
}

#[test]
fn foster_parenting() {
    run(r#"#data
A<table>B<tr>C</tr>D</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     "ABCD"
|     <table>
|       <tbody>
|         <tr>

#data
<table><b>x</b></table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <b>
|       "x"
|     <table>

#data
<table> <tr> <td>a</td> </tr> </table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       " "
|       <tbody>
|         <tr>
|           " "
|           <td>
|             "a"
|           " "
|         " "

#data
<table><div>x</div><tr><td>y
#errors
#document
| <html>
|   <head>
|   <body>
|     <div>
|       "x"
|     <table>
|       <tbody>
|         <tr>
|           <td>
|             "y"
"#);
}

#[test]
fn template_contents() {
    run(r#"#data
<template><tr><td>a</td></tr></template>
#errors
#document
| <html>
|   <head>
|     <template>
|       content
|         <tr>
|           <td>
|             "a"
|   <body>

#data
<body><template><div>x</div></template>
#errors
#document
| <html>
|   <head>
|   <body>
|     <template>
|       content
|         <div>
|           "x"

#data
<template><template><col></template></template>
#errors
#document
| <html>
|   <head>
|     <template>
|       content
|         <template>
|           content
|             <col>
|   <body>
"#);
}

#[test]
fn foreign_content() {
    run(r##"#data
<svg><foreignObject><p>x</p></foreignObject><path/></svg>
#errors
#document
| <html>
|   <head>
|   <body>
|     <svg svg>
|       <svg foreignObject>
|         <p>
|           "x"
|       <svg path>

#data
<svg viewbox="0 0 1 1" xlink:href="#a"><lineargradient/></svg>
#errors
#document
| <html>
|   <head>
|   <body>
|     <svg svg>
|       viewBox="0 0 1 1"
|       xlink href="#a"
|       <svg linearGradient>

#data
<math><mi>x</mi><annotation-xml encoding="text/html"><div>y</div></annotation-xml></math>
#errors
#document
| <html>
|   <head>
|   <body>
|     <math math>
|       <math mi>
|         "x"
|       <math annotation-xml>
|         encoding="text/html"
|         <div>
|           "y"

#data
<math definitionurl="u"></math>
#errors
#document
| <html>
|   <head>
|   <body>
|     <math math>
|       definitionURL="u"

#data
<svg><p>x
#errors
#document
| <html>
|   <head>
|   <body>
|     <svg svg>
|     <p>
|       "x"

#data
<svg><![CDATA[a<b]]></svg>
#errors
#document
| <html>
|   <head>
|   <body>
|     <svg svg>
|       "a<b"

#data
<p><![CDATA[x]]>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       <!-- [CDATA[x]] -->
"##);
}

#[test]
fn quirks_mode() {
    run(r#"#data
<p><table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       <table>

#data
<!DOCTYPE html><p><table>
#errors
#document
| <!DOCTYPE html>
| <html>
|   <head>
|   <body>
|     <p>
|     <table>

#data
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN"><p><table>
#errors
#document
| <!DOCTYPE html "-//W3C//DTD XHTML 1.0 Transitional//EN" "">
| <html>
|   <head>
|   <body>
|     <p>
|     <table>
"#);
    let mode = |text: &str| parse_document(text, true).mode;
    assert_eq!(mode("<p>"), QuirksMode::Quirks);
    assert_eq!(mode("<!DOCTYPE html>"), QuirksMode::NoQuirks);
    assert_eq!(
        mode("<!doctype html system 'about:legacy-compat'>"),
        QuirksMode::NoQuirks
    );
    assert_eq!(
        mode(r#"<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">"#),
        QuirksMode::Quirks
    );
    assert_eq!(
        mode(
            r#"<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN" "http://www.w3.org/TR/html4/loose.dtd">"#
        ),
        QuirksMode::LimitedQuirks
    );
    assert_eq!(mode("<!DOCTYPE html PUBLIC>"), QuirksMode::Quirks);
    assert_eq!(mode("<!DOCTYPE svg>"), QuirksMode::Quirks);

    let mut srcdoc = Document::new_html();
    srcdoc.is_srcdoc = true;
    parse_into(&mut srcdoc, "<p>x", true);
    assert_eq!(srcdoc.mode, QuirksMode::NoQuirks);
}

#[test]
fn fragments() {
    run(r#"#data
<td>a</td><td>b
#errors
#document-fragment
tr
#document
| <td>
|   "a"
| <td>
|   "b"

#data
<path/><foreignObject><div>x</div></foreignObject>
#errors
#document-fragment
svg svg
#document
| <svg path>
| <svg foreignObject>
|   <div>
|     "x"

#data
<b>x</b>&amp;
#errors
#document-fragment
textarea
#document
| "<b>x</b>&"

#data
<td>x
#errors
#document-fragment
template
#document
| <td>
|   "x"

#data
<head></head><body><p>x
#errors
#document-fragment
html
#document
| <head>
| <body>
|   <p>
|     "x"

#data
<option>a<input>
#errors
#document-fragment
select
#document
| <option>
|   "a"

#data
x</p>y
#errors
#document-fragment
div
#document
| "x"
| <p>
| "y"
"#);
}

#[test]
fn scripting_flag_and_form_pointer() {
    run(r#"#data
<body><noscript><p>x</p></noscript>
#errors
#script-on
#document
| <html>
|   <head>
|   <body>
|     <noscript>
|       "<p>x</p>"

#data
<body><noscript><p>x</p></noscript>
#errors
#script-off
#document
| <html>
|   <head>
|   <body>
|     <noscript>
|       <p>
|         "x"

#data
<form id="outer"><div></form><form id="inner"><input>
#errors
#document
| <html>
|   <head>
|   <body>
|     <form>
|       id="outer"
|       <div>
|         <form>
|           id="inner"
|           <input>

#data
<!DOCTYPE html><body><?pi data?><p>&amp;&lt;&notin;&notit;
#errors
#document
| <!DOCTYPE html>
| <html>
|   <head>
|   <body>
|     <?pi data>
|     <p>
|       "&<∉¬it;"
"#);

    let d = parse_document("<form id=a><input id=i></form><form id=b><input>", true);
    let body = d.body().expect("body");
    let forms: Vec<NodeId> = d.children(body).collect();
    let input = d.first_child(forms[0]).expect("input");
    assert_eq!(d.element(input).and_then(|e| e.form_owner), Some(forms[0]));
}

/// The body of `text` serialized. Inputs whose first content belongs in
/// `head` (`script`, `style`, `template`, `noscript`) or before `html` (a
/// comment) start with `<body>` so that it lands in the body.
fn body_html(text: &str) -> String {
    let d = parse_document(text, true);
    serialize_fragment(&d, d.body().expect("body"), true)
}

#[test]
fn serialization_escapes_and_void_elements() {
    assert_eq!(
        body_html("<p a='&quot;&amp;<>'>x&lt;&nbsp;y\"</p><br><img src=x>"),
        "<p a=\"&quot;&amp;&lt;&gt;\">x&lt;&nbsp;y\"</p><br><img src=\"x\">"
    );
    assert_eq!(
        body_html("<textarea>\nfoo</textarea>"),
        "<textarea>foo</textarea>"
    );
    assert_eq!(body_html("<pre>\n\nx</pre>"), "<pre>\nx</pre>");
    assert_eq!(
        body_html("<body><script>a<b && c</script>"),
        "<script>a<b && c</script>"
    );
    assert_eq!(
        body_html("<body><style>p > a {}</style>"),
        "<style>p > a {}</style>"
    );
    assert_eq!(
        body_html("<body><template><p>x</p></template>"),
        "<template><p>x</p></template>"
    );
    assert_eq!(
        body_html("<svg viewBox='0 0 1 1'><foreignObject><b>x</b></foreignObject></svg>"),
        "<svg viewBox=\"0 0 1 1\"><foreignObject><b>x</b></foreignObject></svg>"
    );
    assert_eq!(body_html("<body><!--a--><p>b</p>"), "<!--a--><p>b</p>");
    assert_eq!(
        body_html("<input disabled><hr/><wbr>"),
        "<input disabled=\"\"><hr><wbr>"
    );
    assert_eq!(
        body_html("<body><noscript><b>x</b></noscript>"),
        "<noscript><b>x</b></noscript>"
    );

    // Void elements with children (built through the DOM) serialize no
    // children, and serializing a void element itself gives nothing.
    let mut d = parse_document("<br>", true);
    let body = d.body().expect("body");
    let br = d.first_child(body).expect("br");
    let text = d.create_text("lost");
    d.append_child(br, text);
    assert_eq!(serialize_fragment(&d, body, true), "<br>");
    assert_eq!(serialize_fragment(&d, br, true), "");

    // Text in `noscript` is raw only when scripting is enabled.
    let mut d = Document::new_html();
    let noscript = d.create_html_element("noscript");
    let text = d.create_text("<&>");
    d.append_child(noscript, text);
    let div = d.create_html_element("div");
    d.append_child(div, noscript);
    assert_eq!(
        serialize_fragment(&d, div, true),
        "<noscript><&></noscript>"
    );
    assert_eq!(
        serialize_fragment(&d, div, false),
        "<noscript>&lt;&amp;&gt;</noscript>"
    );
}

#[test]
fn serialization_round_trips_through_the_fragment_parser() {
    let mut d = Document::new_html();
    let context = d.create_html_element("div");
    let markup =
        "<p class=\"a\">x<b>y</b></p><table><tbody><tr><td>1</td></tr></tbody></table><!--c-->";
    let fragment = parse_fragment(&mut d, context, markup, true);
    assert_eq!(serialize_fragment(&d, fragment, true), markup);
}
