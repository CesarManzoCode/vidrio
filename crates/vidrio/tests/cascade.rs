//! The cascade and computed values without a JavaScript realm: the order
//! of cascade layers with `!important` inverted, `revert-layer` and
//! `revert`, `@scope` proximity and limits, inheritance of custom
//! properties (registered and not), and computed values of nested `em`,
//! `rem`, `lh` and `currentcolor`.

use std::cell::RefCell;
use std::rc::Rc;

use vidrio::css::style::{computed_style, resolved_value};
use vidrio::dom::{DocRef, NodeId};

const GREEN: &str = "rgb(0, 128, 0)";
const RED: &str = "rgb(255, 0, 0)";
const BLUE: &str = "rgb(0, 0, 255)";

fn doc(html: &str) -> DocRef {
    Rc::new(RefCell::new(vidrio::html::parse_document(html, true)))
}

fn element(doc: &DocRef, id: &str) -> NodeId {
    let d = doc.borrow();
    d.inclusive_descendants(NodeId::DOCUMENT)
        .into_iter()
        .find(|&n| d.get_attribute(n, "id") == Some(id))
        .unwrap_or_else(|| panic!("no element #{id}"))
}

fn value(doc: &DocRef, id: &str, property: &str) -> String {
    let style = computed_style(doc, element(doc, id)).expect("the element has a style");
    resolved_value(&style, property)
}

#[test]
fn layers_order_normal_and_important_declarations() {
    let d = doc(r#"<style>
        @layer base, theme;
        /* Normal: the later layer wins, whatever the specificity. */
        @layer theme { div { color: green } }
        @layer base { #a#a { color: red } }
        /* Important: the earlier layer wins. */
        @layer theme { #b { color: red !important } }
        @layer base { #b { color: green !important } }
        /* Normal: unlayered declarations win over every layer. */
        #c { color: green }
        @layer theme { #c#c { color: red } }
        /* Important: unlayered declarations lose to every layer. */
        #d#d { color: red !important }
        @layer theme { #d { color: green !important } }
        /* A sublayer comes before its layer's own declarations. */
        @layer theme.inner { #e { color: green } }
        @layer theme { #e#e { color: red } }
        </style>
        <div id=a></div><div id=b></div><div id=c></div><div id=d></div><div id=e></div>"#);
    for id in ["a", "b", "c", "d"] {
        assert_eq!(value(&d, id, "color"), GREEN, "#{id}");
    }
    // A layer's own declarations come after its sublayers.
    assert_eq!(value(&d, "e", "color"), RED);
}

#[test]
fn style_attribute_before_layers() {
    let d = doc(r#"<style>
        @layer base { #a { color: green !important } }
        </style>
        <div id=a style="color: red !important"></div>
        <div id=b style="color: green"></div>
        <style>#b#b#b { color: red }</style>"#);
    // Element-attached declarations are decided before layers: an
    // important style attribute beats an important layered rule.
    assert_eq!(value(&d, "a", "color"), RED);
    // The style attribute beats any normal rule.
    assert_eq!(value(&d, "b", "color"), GREEN);
}

#[test]
fn revert_layer_rolls_back_one_layer() {
    let d = doc(r#"<style>
        @layer one, two;
        @layer one { #a { color: green } }
        @layer two { #a { color: revert-layer } }
        @layer one { #b { color: green } }
        #b { color: revert-layer }
        @layer one { #c { color: revert-layer } }
        @layer one { #d { margin-top: 5px } }
        @layer two { #d { margin-top: 7px !important } }
        @layer one { #d { margin-top: revert-layer !important } }
        div { display: inline }
        #e { display: revert }
        </style>
        <div id=a></div><div id=b></div>
        <section style="color: blue"><span id=c></span></section>
        <div id=d></div><div id=e></div><div id=f></div>"#);
    assert_eq!(value(&d, "a", "color"), GREEN);
    assert_eq!(value(&d, "b", "color"), GREEN);
    // No lower layer: back to the user agent origin, which sets nothing,
    // so `color` inherits.
    assert_eq!(value(&d, "c", "color"), BLUE);
    // An important `revert-layer` rolls back to the important
    // declarations of the next layer.
    assert_eq!(value(&d, "d", "margin-top"), "7px");
    // `revert` goes back to the user agent sheet.
    assert_eq!(value(&d, "e", "display"), "block");
    assert_eq!(value(&d, "f", "display"), "inline");
}

#[test]
fn scope_proximity_decides_after_specificity() {
    let d = doc(r#"<style>
        @scope (.light) { p { color: green } }
        @scope (.dark) { p { color: red } }
        @scope (.far) { .x { color: blue } }
        @scope (.near) { p { color: red } }
        @scope (.card) to (.content) { span { color: green } }
        </style>
        <div class=dark><div class=light><p id=a></p></div></div>
        <div class=light><div class=dark><p id=b></p></div></div>
        <div class=far><div class=near><p id=c class=x></p></div></div>
        <div class=card><span id=d></span><div class=content><span id=e></span></div></div>"#);
    // The nearer scoping root wins over the order of appearance.
    assert_eq!(value(&d, "a", "color"), GREEN);
    assert_eq!(value(&d, "b", "color"), RED);
    // Specificity decides before proximity.
    assert_eq!(value(&d, "c", "color"), BLUE);
    // The scoping limit and what is below it are out of scope.
    assert_eq!(value(&d, "d", "color"), GREEN);
    assert_ne!(value(&d, "e", "color"), GREEN);
}

#[test]
fn scope_root_and_implicit_root() {
    let d = doc(r#"<div id=host><style>
        @scope { :scope { color: green } p { color: blue } }
        </style><p id=p></p></div><p id=outside></p>"#);
    // Without `<scope-start>` the root is the parent of the owner node.
    assert_eq!(value(&d, "host", "color"), GREEN);
    assert_eq!(value(&d, "p", "color"), BLUE);
    assert_ne!(value(&d, "outside", "color"), BLUE);
}

#[test]
fn custom_properties_inherit_and_substitute() {
    let d = doc(r#"<style>
        @property --n { syntax: "<length>"; inherits: false; initial-value: 3px; }
        @property --m { syntax: "<length>"; inherits: true; initial-value: 0px; }
        #a { --x: 10px; --y: var(--x) 2; --n: 7px; --m: 2em; font-size: 10px }
        #b { --x: 20px; margin-left: var(--x); font-size: 40px }
        #c { padding-left: var(--x); margin-top: var(--missing, 4px); margin-bottom: var(--missing) }
        #cycle { --p: var(--q); --q: var(--p); margin-top: var(--p, 9px) }
        </style>
        <div id=a><div id=b><div id=c></div><div id=cycle></div></div></div>"#);
    // An unregistered property inherits the value computed on the parent
    // (`var()` substituted there).
    assert_eq!(value(&d, "b", "--y"), "10px 2");
    assert_eq!(value(&d, "b", "--x"), "20px");
    assert_eq!(value(&d, "b", "margin-left"), "20px");
    assert_eq!(value(&d, "c", "--x"), "20px");
    assert_eq!(value(&d, "c", "padding-left"), "20px");
    assert_eq!(value(&d, "c", "margin-top"), "4px");
    // Invalid at computed-value time: the initial value.
    assert_eq!(value(&d, "c", "margin-bottom"), "0px");
    // A registered property that does not inherit takes its initial
    // value; one that inherits keeps the absolute length of the parent.
    assert_eq!(value(&d, "a", "--n"), "7px");
    assert_eq!(value(&d, "b", "--n"), "3px");
    assert_eq!(value(&d, "a", "--m"), "20px");
    assert_eq!(value(&d, "b", "--m"), "20px");
    // Properties on a cycle are invalid: the fallback applies.
    assert_eq!(value(&d, "cycle", "--p"), "");
    assert_eq!(value(&d, "cycle", "margin-top"), "9px");
}

#[test]
fn nested_em_rem_and_lh() {
    let d = doc(r#"<style>
        html { font-size: 20px }
        #a { font-size: 10px }
        #b { font-size: 1.5em; margin-top: 2em }
        #c { font-size: 0.5em; padding-left: 1rem; line-height: 2; margin-left: 1lh }
        #d { font-size: 200%; line-height: 1.5em }
        #e { font-size: larger }
        </style>
        <div id=a><div id=b><div id=c><div id=d><div id=e></div></div></div></div></div>"#);
    assert_eq!(value(&d, "b", "font-size"), "15px");
    assert_eq!(value(&d, "b", "margin-top"), "30px");
    assert_eq!(value(&d, "c", "font-size"), "7.5px");
    assert_eq!(value(&d, "c", "padding-left"), "20px");
    assert_eq!(value(&d, "c", "line-height"), "2");
    assert_eq!(value(&d, "c", "margin-left"), "15px");
    // `em` in `line-height` is the element's own font size; the number
    // `2` inherits as a number, the length as a length.
    assert_eq!(value(&d, "d", "font-size"), "15px");
    assert_eq!(value(&d, "d", "line-height"), "22.5px");
    assert_eq!(value(&d, "e", "font-size"), "18px");
    assert_eq!(value(&d, "e", "line-height"), "22.5px");
}

#[test]
fn currentcolor_computes_against_the_element() {
    let d = doc(r#"<style>
        #a { color: rgb(0, 0, 255); border-top-style: solid; border-top-color: currentcolor;
             caret-color: currentcolor }
        #b { color: rgb(255, 0, 0) }
        #c { color: currentcolor }
        #d { color: inherit; background-color: currentcolor }
        </style>
        <div id=a><div id=b></div><div id=c></div><div id=d></div></div>"#);
    assert_eq!(value(&d, "a", "border-top-color"), BLUE);
    assert_eq!(value(&d, "a", "caret-color"), BLUE);
    // `caret-color` inherits `currentcolor`, which takes the child's
    // color.
    assert_eq!(value(&d, "b", "caret-color"), RED);
    // `currentcolor` on `color` is the parent's color.
    assert_eq!(value(&d, "c", "color"), BLUE);
    assert_eq!(value(&d, "d", "background-color"), BLUE);
    // The initial border color is `currentcolor` too.
    assert_eq!(value(&d, "b", "border-left-color"), RED);
    // A border side whose style is `none` has no width.
    assert_eq!(value(&d, "b", "border-left-width"), "0px");
    assert_eq!(value(&d, "a", "border-top-width"), "3px");
}

#[test]
fn font_white_space_and_text_wrap_reach_their_longhands() {
    let d = doc(concat!(
        "<style>#f { font: italic bold 20px/2 Ahem, serif }",
        "#g { font: 10px monospace; font-weight: 300 }",
        "#w { white-space: pre } #x { white-space: pre-line }",
        "#t { text-wrap: nowrap balance } #bad { font: 10px; white-space: pre pre }</style>",
        "<div id=f></div><div id=g></div><div id=w></div><div id=x></div>",
        "<div id=t></div><div id=bad></div>"
    ));
    assert_eq!(value(&d, "f", "font-size"), "20px");
    assert_eq!(value(&d, "f", "line-height"), "2");
    assert_eq!(value(&d, "f", "font-weight"), "700");
    assert_eq!(value(&d, "f", "font-style"), "italic");
    assert_eq!(value(&d, "f", "font-family"), "Ahem, serif");
    assert_eq!(value(&d, "g", "font-size"), "10px");
    assert_eq!(value(&d, "g", "font-weight"), "300");
    assert_eq!(value(&d, "g", "line-height"), "normal");
    assert_eq!(value(&d, "w", "white-space-collapse"), "preserve");
    assert_eq!(value(&d, "w", "text-wrap-mode"), "nowrap");
    assert_eq!(value(&d, "x", "white-space-collapse"), "preserve-breaks");
    assert_eq!(value(&d, "x", "text-wrap-mode"), "wrap");
    assert_eq!(value(&d, "t", "text-wrap-mode"), "nowrap");
    assert_eq!(value(&d, "t", "text-wrap-style"), "balance");
    // Invalid values set nothing: the initial values stay.
    assert_eq!(value(&d, "bad", "font-size"), "16px");
    assert_eq!(value(&d, "bad", "white-space-collapse"), "collapse");
}
