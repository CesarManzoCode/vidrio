//! CSS values without a JavaScript realm: simplification and
//! serialization of math functions with mixed types, conversions between
//! color spaces against the reference values of CSS Color 4 (D50 Lab with
//! Bradford adaptation, OKLab), `color-mix()` and the relative color
//! syntax, `var()` with fallbacks and cycles, `attr()`, and registered
//! custom properties.

use std::collections::HashMap;

use vidrio::css::declaration::{DeclarationBlock, serialize_tokens};
use vidrio::css::parser::parse_component_values;
use vidrio::css::tokenizer::{Token, tokenize};
use vidrio::css::values::color::{self, Absolute, Color, ColorContext, Space};
use vidrio::css::values::compute::{self, Context};
use vidrio::css::values::custom::{self, Registration, RegistrationError, Registry, Substitution};
use vidrio::css::values::{Numeric, parse_numeric};

// ----- math functions --------------------------------------------------------------------

fn math(text: &str, ty: Numeric) -> Option<String> {
    let cv = parse_component_values(text);
    parse_numeric(&cv[0], ty, None).map(|v| v.to_css())
}

#[test]
fn calc_combines_units_and_sorts_its_terms() {
    let lp = Numeric::LengthPercentage;
    assert_eq!(
        math("calc(1px + 2em + 3px)", lp).as_deref(),
        Some("calc(2em + 4px)")
    );
    assert_eq!(
        math("calc(10% - 5px)", lp).as_deref(),
        Some("calc(10% - 5px)")
    );
    assert_eq!(
        math("calc((1in + 2cm) * 2)", lp).as_deref(),
        Some("calc(343.181px)")
    );
    assert_eq!(
        math("calc(1px * 2 / 4)", lp).as_deref(),
        Some("calc(0.5px)")
    );
    assert_eq!(
        math("calc(2em - 1em + 10%)", lp).as_deref(),
        Some("calc(10% + 1em)")
    );
    assert_eq!(
        math("min(10px, 1em, 5px)", lp).as_deref(),
        Some("min(5px, 1em)")
    );
    assert_eq!(
        math("clamp(1px, 2px, 3px)", lp).as_deref(),
        Some("calc(2px)")
    );
    assert_eq!(math("max(1px, 10%)", lp).as_deref(), Some("max(1px, 10%)"));
}

#[test]
fn calc_types_mixed_dimensions() {
    // length / length is a number; it stays unsimplified with a relative
    // unit.
    assert_eq!(
        math("calc(100px / 2em)", Numeric::Number).as_deref(),
        Some("calc(100px / 2em)")
    );
    assert_eq!(
        math("calc(100px / 4px)", Numeric::Number).as_deref(),
        Some("calc(25)")
    );
    assert_eq!(
        math("calc(1turn / 4)", Numeric::Angle).as_deref(),
        Some("calc(90deg)")
    );
    assert_eq!(
        math("calc(1s + 500ms)", Numeric::Time).as_deref(),
        Some("calc(1.5s)")
    );
    // Invalid types.
    assert_eq!(math("calc(1px + 1s)", Numeric::Length), None);
    assert_eq!(math("calc(1px * 1px)", Numeric::Length), None);
    assert_eq!(math("calc(1px + 1)", Numeric::Length), None);
    assert_eq!(math("calc(10%)", Numeric::Length), None);
}

#[test]
fn math_functions_simplify() {
    let l = Numeric::Length;
    assert_eq!(
        math("calc(sign(-5px) * 3px)", l).as_deref(),
        Some("calc(-3px)")
    );
    assert_eq!(math("round(up, 7px, 2px)", l).as_deref(), Some("calc(8px)"));
    assert_eq!(math("mod(-7px, 3px)", l).as_deref(), Some("calc(2px)"));
    assert_eq!(math("rem(-7px, 3px)", l).as_deref(), Some("calc(-1px)"));
    assert_eq!(math("hypot(3px, 4px)", l).as_deref(), Some("calc(5px)"));
    assert_eq!(
        math("calc(pow(2, 3) * 1px)", l).as_deref(),
        Some("calc(8px)")
    );
    assert_eq!(
        math("calc(sqrt(16) * 1px)", l).as_deref(),
        Some("calc(4px)")
    );
    assert_eq!(math("abs(-2px)", l).as_deref(), Some("calc(2px)"));
    assert_eq!(math("calc(cos(0) * 1px)", l).as_deref(), Some("calc(1px)"));
    assert_eq!(math("calc(1px * log(e))", l).as_deref(), Some("calc(1px)"));
    assert_eq!(
        math("atan2(1px, 1px)", Numeric::Angle).as_deref(),
        Some("calc(45deg)")
    );
    assert_eq!(
        math("calc(infinity * 1px)", l).as_deref(),
        Some("calc(infinity * 1px)")
    );
    assert_eq!(
        math("round(1em, 2px)", l).as_deref(),
        Some("round(1em, 2px)")
    );
}

#[test]
fn calc_computes_against_the_element() {
    let ctx = Context {
        font_size: 10.0,
        ..Context::default()
    };
    let computed = |name: &str, value: &str| compute::computed_text(name, &tokenize(value), &ctx);
    assert_eq!(
        computed("width", "calc(10% + 2em - 1em)").as_deref(),
        Some("calc(10% + 10px)")
    );
    assert_eq!(
        computed("width", "calc(1vw + 2px)").as_deref(),
        Some("10px")
    );
    assert_eq!(computed("width", "min(1em, 20px)").as_deref(), Some("10px"));
    assert_eq!(computed("order", "calc(3.5)").as_deref(), Some("4"));
}

#[test]
fn declarations_keep_their_canonical_specified_value() {
    let b = DeclarationBlock::parse(
        "width: calc(1px + 2px); color: #ABC; margin-left: 0; height: calc(1px + 1s); \
         background-image: linear-gradient(to right, red, #00f)",
    );
    assert_eq!(
        b.serialize(),
        "width: calc(3px); color: rgb(170, 187, 204); margin-left: 0px; \
         background-image: linear-gradient(to right, red, rgb(0, 0, 255));"
    );
}

// ----- colors ----------------------------------------------------------------------------

fn absolute(text: &str) -> Absolute {
    match color::parse_str(text)
        .expect(text)
        .compute(&ColorContext::default())
    {
        Color::Absolute(a) => a,
        other => panic!("{text} did not compute: {other:?}"),
    }
}

fn computed_color(text: &str) -> String {
    color::parse_str(text)
        .expect(text)
        .compute(&ColorContext::default())
        .to_css()
}

fn assert_close(got: Absolute, want: [f64; 3], tolerance: f64) {
    for (i, w) in want.iter().enumerate() {
        let v = got.c[i].expect("a component");
        assert!(
            (v - w).abs() < tolerance,
            "component {i}: {v} vs {w} in {got:?}"
        );
    }
}

#[test]
fn conversions_match_css_color_4_reference_values() {
    let red = absolute("red");
    assert_close(
        red.convert(Space::XyzD65),
        [0.412391, 0.212639, 0.019331],
        1e-5,
    );
    assert_close(red.convert(Space::Lab), [54.2905, 80.8049, 69.8910], 1e-3);
    assert_close(red.convert(Space::Lch), [54.2905, 106.8372, 40.8577], 1e-3);
    assert_close(
        red.convert(Space::Oklab),
        [0.627955, 0.224863, 0.125846],
        1e-5,
    );
    assert_close(
        red.convert(Space::Oklch),
        [0.627955, 0.257683, 29.2339],
        1e-3,
    );
    assert_close(
        red.convert(Space::DisplayP3),
        [0.917488, 0.200287, 0.138561],
        1e-5,
    );
    assert_close(
        absolute("white").convert(Space::Lab),
        [100.0, 0.0, 0.0],
        1e-3,
    );
    // Back to sRGB.
    let s = absolute("lab(54.2905 80.8049 69.8910)").to_srgb();
    assert!((s[0] - 1.0).abs() < 1e-4 && s[1].abs() < 1e-3 && s[2].abs() < 1e-3);
    assert_close(
        absolute("oklch(0.7 0.1 200)").convert(Space::Oklab),
        [0.7, -0.0939693, -0.0342020],
        1e-6,
    );
    // An achromatic color has a powerless (missing) hue.
    assert_eq!(absolute("gray").convert(Space::Oklch).c[2], None);
}

#[test]
fn colors_serialize_by_their_syntax() {
    assert_eq!(computed_color("hsl(30 50% 50%)"), "rgb(191, 128, 64)");
    assert_eq!(computed_color("hwb(120 20% 20%)"), "rgb(51, 204, 51)");
    assert_eq!(computed_color("rgb(300 -10 128 / 2)"), "rgb(255, 0, 128)");
    assert_eq!(
        computed_color("hsla(0, 100%, 50%, 0.25)"),
        "rgba(255, 0, 0, 0.25)"
    );
    assert_eq!(computed_color("lab(150 0 0 / 0.5)"), "lab(100 0 0 / 0.5)");
    assert_eq!(
        computed_color("oklch(50% 0.2 -60deg)"),
        "oklch(0.5 0.2 300)"
    );
    assert_eq!(
        computed_color("color(display-p3 1 0 none)"),
        "color(display-p3 1 0 none)"
    );
    assert_eq!(computed_color("light-dark(red, blue)"), "rgb(255, 0, 0)");
    assert_eq!(
        color::parse_str("CanvasText").unwrap().to_css(),
        "canvastext"
    );
    assert!(color::parse_str("rgb(1 2)").is_none());
    assert!(color::parse_str("lab(1, 2, 3)").is_none());
    assert!(color::parse_str("color(unknown 1 2 3)").is_none());
}

#[test]
fn color_mix_interpolates_with_premultiplied_alpha() {
    assert_eq!(
        computed_color("color-mix(in srgb, red, blue)"),
        "color(srgb 0.5 0 0.5)"
    );
    assert_eq!(
        computed_color("color-mix(in srgb, red 25%, blue)"),
        "color(srgb 0.25 0 0.75)"
    );
    assert_eq!(
        computed_color("color-mix(in srgb, red 30%, blue 30%)"),
        "color(srgb 0.5 0 0.5 / 0.6)"
    );
    // Shorter hue: 0deg and 240deg meet at 300deg.
    assert_eq!(
        computed_color("color-mix(in hsl, red, blue)"),
        "rgb(255, 0, 255)"
    );
    assert_eq!(
        computed_color("color-mix(in hsl longer hue, red, blue)"),
        "rgb(0, 255, 0)"
    );
    // A missing component takes the other color's value.
    assert_eq!(
        computed_color("color-mix(in srgb, rgb(none 0 0), red)"),
        "color(srgb 1 0 0)"
    );
    // Premultiplied: transparent does not darken the mix.
    assert_eq!(
        computed_color("color-mix(in srgb, transparent, red)"),
        "color(srgb 1 0 0 / 0.5)"
    );
    let mid = absolute("color-mix(in oklab, white, black)");
    assert_eq!(mid.space, Space::Oklab);
    assert_close(mid, [0.5, 0.0, 0.0], 1e-6);
    // Specified values keep the function, with normalized percentages.
    assert_eq!(
        color::parse_str("color-mix(in srgb, red 50%, blue 50%)")
            .unwrap()
            .to_css(),
        "color-mix(in srgb, red, blue)"
    );
    assert_eq!(
        color::parse_str("color-mix(in lch, red 30%, blue 70%)")
            .unwrap()
            .to_css(),
        "color-mix(in lch, red 30%, blue)"
    );
    assert!(color::parse_str("color-mix(in srgb, red 0%, blue 0%)").is_none());
    // currentcolor keeps the mix until it is known.
    assert_eq!(
        computed_color("color-mix(in srgb, currentcolor, red)"),
        "color-mix(in srgb, currentcolor, rgb(255, 0, 0))"
    );
}

#[test]
fn relative_colors_resolve_their_channels() {
    assert_eq!(
        computed_color("rgb(from rebeccapurple r g b)"),
        "color(srgb 0.4 0.2 0.6)"
    );
    assert_eq!(
        computed_color("rgb(from rebeccapurple calc(r * 2) g b / 50%)"),
        "color(srgb 0.8 0.2 0.6 / 0.5)"
    );
    assert_eq!(
        computed_color("hsl(from red calc(h + 120) s l)"),
        "color(srgb 0 1 0)"
    );
    assert_eq!(
        computed_color("rgb(from rgb(10 20 30 / 0.5) r g b / alpha)"),
        "color(srgb 0.0392157 0.0784314 0.117647 / 0.5)"
    );
    assert_eq!(
        computed_color("color(from red srgb r g none)"),
        "color(srgb 1 0 none)"
    );
    assert_close(
        absolute("lab(from red l a b)"),
        [54.2905, 80.8049, 69.8910],
        1e-3,
    );
    assert_eq!(
        color::parse_str("rgb(from red r g b)").unwrap().to_css(),
        "rgb(from red r g b)"
    );
    // Channel keywords belong to their function.
    assert!(color::parse_str("rgb(from red h s l)").is_none());
    // currentcolor resolves when it is known.
    let ctx = ColorContext {
        current: Some(absolute("blue")),
        dark: false,
    };
    assert_eq!(
        color::parse_str("rgb(from currentcolor b g r)")
            .unwrap()
            .compute(&ctx)
            .to_css(),
        "color(srgb 1 0 0)"
    );
}

// ----- custom properties -----------------------------------------------------------------

fn no_attr(_: &str) -> Option<String> {
    None
}

fn specified(decls: &[(&str, &str)]) -> Vec<(String, Vec<Token>)> {
    decls
        .iter()
        .map(|(n, v)| (n.to_string(), tokenize(v)))
        .collect()
}

fn text(map: &HashMap<String, Vec<Token>>, name: &str) -> Option<String> {
    map.get(name).map(|v| custom::serialize_value(v))
}

#[test]
fn var_substitutes_with_fallbacks_and_cycles() {
    let decls = specified(&[
        ("--a", "10px"),
        ("--b", "var(--a) var(--a)"),
        ("--c", "var(--missing, fallback var(--a))"),
        ("--d", "var(--missing)"),
        ("--e", "var(--f)"),
        ("--f", "var(--e)"),
        ("--g", "var(--e, ok)"),
        ("--h", "initial"),
    ]);
    let mut parent = HashMap::new();
    parent.insert("--h".to_string(), tokenize("inherited"));
    parent.insert("--i".to_string(), tokenize("from parent"));
    let out = custom::compute_custom_properties(
        &decls,
        &parent,
        &Registry::default(),
        &Context::default(),
        &no_attr,
    );
    assert_eq!(text(&out, "--b").as_deref(), Some("10px 10px"));
    assert_eq!(text(&out, "--c").as_deref(), Some("fallback 10px"));
    assert_eq!(text(&out, "--d"), None);
    assert_eq!(text(&out, "--e"), None);
    assert_eq!(text(&out, "--f"), None);
    assert_eq!(text(&out, "--g").as_deref(), Some("ok"));
    assert_eq!(text(&out, "--h"), None);
    assert_eq!(text(&out, "--i").as_deref(), Some("from parent"));
    let width =
        custom::substitute_with(&tokenize("calc(var(--a) * 2)"), &out, &no_attr).expect("valid");
    assert_eq!(serialize_tokens(&width), "calc(10px * 2)");
    assert!(custom::substitute_with(&tokenize("var(--d)"), &out, &no_attr).is_err());
}

#[test]
fn attr_substitutes_typed_attribute_values() {
    let attrs = |name: &str| match name {
        "data-size" => Some("12".to_string()),
        "title" => Some("hi".to_string()),
        "data-x" => Some("bogus".to_string()),
        "data-len" => Some("2em".to_string()),
        _ => None,
    };
    let none = |_: &str| -> Option<Vec<Token>> { None };
    let s = Substitution {
        var: &none,
        attr: &attrs,
    };
    let sub = |v: &str| custom::substitute(&tokenize(v), &s).map(|t| serialize_tokens(&t));
    assert_eq!(sub("attr(data-size px, 1px)").as_deref(), Ok("12px"));
    assert_eq!(sub("attr(data-size %)").as_deref(), Ok("12%"));
    assert_eq!(sub("attr(title)").as_deref(), Ok("\"hi\""));
    assert_eq!(
        sub("attr(data-x type(<length>), 3px)").as_deref(),
        Ok("3px")
    );
    assert_eq!(sub("attr(data-len type(<length>))").as_deref(), Ok("2em"));
    assert!(sub("attr(missing)").is_err());
}

#[test]
fn registered_properties_are_typed() {
    let mut registry = Registry::default();
    let len = Registration::new("--len", "<length>", false, Some("calc(1in + 4px)"))
        .expect("a valid registration");
    assert_eq!(serialize_tokens(len.initial.as_ref().unwrap()), "100px");
    registry.register(len.clone()).expect("registered");
    assert_eq!(
        registry.register(len),
        Err(RegistrationError::InvalidModification)
    );
    registry
        .register(Registration::new("--color", "<color>", true, Some("red")).unwrap())
        .unwrap();
    registry
        .register(Registration::new("--list", "<integer>#", false, Some("1, 2")).unwrap())
        .unwrap();
    assert_eq!(
        Registration::new("--x", "<length>", false, Some("1em")),
        Err(RegistrationError::Syntax)
    );
    assert_eq!(
        Registration::new("--x", "<length>", false, None),
        Err(RegistrationError::Syntax)
    );
    assert_eq!(
        Registration::new("--x", "<length> | auto", false, Some("var(--y)")),
        Err(RegistrationError::Syntax)
    );
    assert!(Registration::new("--x", "*", false, None).is_ok());

    let ctx = Context {
        font_size: 20.0,
        ..Context::default()
    };
    let mut parent = HashMap::new();
    parent.insert("--len".to_string(), tokenize("5px"));
    parent.insert("--color".to_string(), tokenize("rgb(0, 0, 255)"));
    let out = custom::compute_custom_properties(
        &specified(&[
            ("--list", "calc(1.5), 3"),
            ("--uses", "var(--len) var(--color)"),
        ]),
        &parent,
        &registry,
        &ctx,
        &no_attr,
    );
    // Not inherited: the initial value; inherited: the parent's.
    assert_eq!(text(&out, "--len").as_deref(), Some("100px"));
    assert_eq!(text(&out, "--color").as_deref(), Some("rgb(0, 0, 255)"));
    assert_eq!(text(&out, "--list").as_deref(), Some("2, 3"));
    assert_eq!(
        text(&out, "--uses").as_deref(),
        Some("100px rgb(0, 0, 255)")
    );

    let out = custom::compute_custom_properties(
        &specified(&[("--len", "2em"), ("--color", "var(--len)")]),
        &HashMap::new(),
        &registry,
        &ctx,
        &no_attr,
    );
    // Computed with the element's font; an invalid value falls back to
    // the initial value.
    assert_eq!(text(&out, "--len").as_deref(), Some("40px"));
    assert_eq!(text(&out, "--color").as_deref(), Some("rgb(255, 0, 0)"));
}
