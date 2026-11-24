//! The CSS properties CSSOM knows by name: the longhands, the shorthands
//! with their longhands and the initial values the expansion of a
//! shorthand gives to the longhands it does not mention, legacy name
//! aliases, the descriptors of `@font-face`, `@page` and `@property`, and
//! the IDL attribute names of `CSSStyleProperties` (CSSOM §6.7.1).
//!
//! The value grammar of each property comes with the CSS values sprint;
//! here a value is only checked to be a well-formed list of component
//! values (LIMITATIONS.md).

/// The longhand properties (and shorthands without an expansion here,
/// which are kept as one declaration).
pub const LONGHANDS: &[&str] = &[
    "accent-color",
    "align-content",
    "align-items",
    "align-self",
    "alignment-baseline",
    "anchor-name",
    "animation-composition",
    "animation-delay",
    "animation-direction",
    "animation-duration",
    "animation-fill-mode",
    "animation-iteration-count",
    "animation-name",
    "animation-play-state",
    "animation-timing-function",
    "appearance",
    "aspect-ratio",
    "backdrop-filter",
    "backface-visibility",
    "background-attachment",
    "background-blend-mode",
    "background-clip",
    "background-color",
    "background-image",
    "background-origin",
    "background-position-x",
    "background-position-y",
    "background-repeat",
    "background-size",
    "baseline-shift",
    "baseline-source",
    "block-size",
    "border-block-end-color",
    "border-block-end-style",
    "border-block-end-width",
    "border-block-start-color",
    "border-block-start-style",
    "border-block-start-width",
    "border-bottom-color",
    "border-bottom-left-radius",
    "border-bottom-right-radius",
    "border-bottom-style",
    "border-bottom-width",
    "border-collapse",
    "border-end-end-radius",
    "border-end-start-radius",
    "border-image-outset",
    "border-image-repeat",
    "border-image-slice",
    "border-image-source",
    "border-image-width",
    "border-inline-end-color",
    "border-inline-end-style",
    "border-inline-end-width",
    "border-inline-start-color",
    "border-inline-start-style",
    "border-inline-start-width",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-spacing",
    "border-start-end-radius",
    "border-start-start-radius",
    "border-top-color",
    "border-top-left-radius",
    "border-top-right-radius",
    "border-top-style",
    "border-top-width",
    "bottom",
    "box-decoration-break",
    "box-shadow",
    "box-sizing",
    "break-after",
    "break-before",
    "break-inside",
    "caption-side",
    "caret-color",
    "clear",
    "clip",
    "clip-path",
    "clip-rule",
    "color",
    "color-interpolation",
    "color-interpolation-filters",
    "color-scheme",
    "column-count",
    "column-fill",
    "column-gap",
    "column-rule-color",
    "column-rule-style",
    "column-rule-width",
    "column-span",
    "column-width",
    "contain",
    "contain-intrinsic-block-size",
    "contain-intrinsic-height",
    "contain-intrinsic-inline-size",
    "contain-intrinsic-width",
    "container-name",
    "container-type",
    "content",
    "content-visibility",
    "counter-increment",
    "counter-reset",
    "counter-set",
    "cursor",
    "cx",
    "cy",
    "d",
    "direction",
    "display",
    "dominant-baseline",
    "empty-cells",
    "fill",
    "fill-opacity",
    "fill-rule",
    "filter",
    "flex-basis",
    "flex-direction",
    "flex-grow",
    "flex-shrink",
    "flex-wrap",
    "float",
    "flood-color",
    "flood-opacity",
    "font-family",
    "font-feature-settings",
    "font-kerning",
    "font-language-override",
    "font-optical-sizing",
    "font-palette",
    "font-size",
    "font-size-adjust",
    "font-stretch",
    "font-style",
    "font-synthesis-small-caps",
    "font-synthesis-style",
    "font-synthesis-weight",
    "font-variant-alternates",
    "font-variant-caps",
    "font-variant-east-asian",
    "font-variant-emoji",
    "font-variant-ligatures",
    "font-variant-numeric",
    "font-variant-position",
    "font-variation-settings",
    "font-weight",
    "forced-color-adjust",
    "grid-auto-columns",
    "grid-auto-flow",
    "grid-auto-rows",
    "grid-column-end",
    "grid-column-start",
    "grid-row-end",
    "grid-row-start",
    "grid-template-areas",
    "grid-template-columns",
    "grid-template-rows",
    "hanging-punctuation",
    "height",
    "hyphenate-character",
    "hyphens",
    "image-orientation",
    "image-rendering",
    "inline-size",
    "inset-block-end",
    "inset-block-start",
    "inset-inline-end",
    "inset-inline-start",
    "isolation",
    "justify-content",
    "justify-items",
    "justify-self",
    "left",
    "letter-spacing",
    "lighting-color",
    "line-break",
    "line-height",
    "list-style-image",
    "list-style-position",
    "list-style-type",
    "margin-block-end",
    "margin-block-start",
    "margin-bottom",
    "margin-inline-end",
    "margin-inline-start",
    "margin-left",
    "margin-right",
    "margin-top",
    "marker-end",
    "marker-mid",
    "marker-start",
    "mask-clip",
    "mask-composite",
    "mask-image",
    "mask-mode",
    "mask-origin",
    "mask-position",
    "mask-repeat",
    "mask-size",
    "mask-type",
    "math-depth",
    "math-shift",
    "math-style",
    "max-block-size",
    "max-height",
    "max-inline-size",
    "max-width",
    "min-block-size",
    "min-height",
    "min-inline-size",
    "min-width",
    "mix-blend-mode",
    "object-fit",
    "object-position",
    "offset-anchor",
    "offset-distance",
    "offset-path",
    "offset-position",
    "offset-rotate",
    "opacity",
    "order",
    "orphans",
    "outline-color",
    "outline-offset",
    "outline-style",
    "outline-width",
    "overflow-anchor",
    "overflow-block",
    "overflow-clip-margin",
    "overflow-inline",
    "overflow-wrap",
    "overflow-x",
    "overflow-y",
    "overscroll-behavior-block",
    "overscroll-behavior-inline",
    "overscroll-behavior-x",
    "overscroll-behavior-y",
    "padding-block-end",
    "padding-block-start",
    "padding-bottom",
    "padding-inline-end",
    "padding-inline-start",
    "padding-left",
    "padding-right",
    "padding-top",
    "page",
    "paint-order",
    "perspective",
    "perspective-origin",
    "pointer-events",
    "position",
    "position-anchor",
    "print-color-adjust",
    "quotes",
    "r",
    "resize",
    "right",
    "rotate",
    "row-gap",
    "ruby-align",
    "ruby-position",
    "rx",
    "ry",
    "scale",
    "scroll-behavior",
    "scroll-margin-block-end",
    "scroll-margin-block-start",
    "scroll-margin-bottom",
    "scroll-margin-inline-end",
    "scroll-margin-inline-start",
    "scroll-margin-left",
    "scroll-margin-right",
    "scroll-margin-top",
    "scroll-padding-block-end",
    "scroll-padding-block-start",
    "scroll-padding-bottom",
    "scroll-padding-inline-end",
    "scroll-padding-inline-start",
    "scroll-padding-left",
    "scroll-padding-right",
    "scroll-padding-top",
    "scroll-snap-align",
    "scroll-snap-stop",
    "scroll-snap-type",
    "scrollbar-color",
    "scrollbar-gutter",
    "scrollbar-width",
    "shape-image-threshold",
    "shape-margin",
    "shape-outside",
    "shape-rendering",
    "stop-color",
    "stop-opacity",
    "stroke",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-opacity",
    "stroke-width",
    "tab-size",
    "table-layout",
    "text-align",
    "text-align-last",
    "text-anchor",
    "text-combine-upright",
    "text-decoration-color",
    "text-decoration-line",
    "text-decoration-skip-ink",
    "text-decoration-style",
    "text-decoration-thickness",
    "text-emphasis-color",
    "text-emphasis-position",
    "text-emphasis-style",
    "text-indent",
    "text-justify",
    "text-orientation",
    "text-overflow",
    "text-rendering",
    "text-shadow",
    "text-size-adjust",
    "text-transform",
    "text-underline-offset",
    "text-underline-position",
    "text-wrap-mode",
    "text-wrap-style",
    "top",
    "touch-action",
    "transform",
    "transform-box",
    "transform-origin",
    "transform-style",
    "transition-behavior",
    "transition-delay",
    "transition-duration",
    "transition-property",
    "transition-timing-function",
    "translate",
    "unicode-bidi",
    "user-select",
    "vector-effect",
    "vertical-align",
    "view-transition-name",
    "visibility",
    "white-space-collapse",
    "widows",
    "width",
    "will-change",
    "word-break",
    "word-spacing",
    "writing-mode",
    "x",
    "y",
    "z-index",
    "zoom",
    // Shorthands kept as one declaration (no expansion yet).
    "all",
    "animation",
    "background",
    "background-position",
    "border-image",
    "container",
    "contain-intrinsic-size",
    "font",
    "font-synthesis",
    "font-variant",
    "grid",
    "grid-template",
    "mask",
    "marker",
    "offset",
    "text-emphasis",
    "text-wrap",
    "transition",
    "white-space",
];

/// A shorthand with an expansion: its name, how its value is written and
/// its longhands with their initial values, in canonical order.
pub struct Shorthand {
    pub name: &'static str,
    pub kind: Kind,
    pub longhands: &'static [(&'static str, &'static str)],
}

/// How a shorthand's value maps to its longhands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One to four values: top, right, bottom, left.
    Box,
    /// One or two values: the second defaults to the first.
    Pair,
    /// A width, a style and a color in any order (`border-top`).
    Side,
    /// `border`: a side for all four sides, and resets `border-image`.
    Border,
    /// `border-block`/`border-inline`: a side for both of its sides.
    SidePair,
    /// `border-radius`: one to four values, an optional `/` and one to
    /// four more.
    Radius,
    /// `flex`.
    Flex,
    /// `flex-flow`: a direction and a wrap in any order.
    FlexFlow,
    /// `list-style`: a position, an image and a type in any order.
    ListStyle,
    /// `text-decoration`: line, style, color and thickness.
    TextDecoration,
    /// `columns`: a width and a count in any order.
    Columns,
    /// `grid-row`/`grid-column`: start `/` end.
    GridLine,
    /// `grid-area`: up to four lines separated by `/`.
    GridArea,
}

const fn sh(
    name: &'static str,
    kind: Kind,
    longhands: &'static [(&'static str, &'static str)],
) -> Shorthand {
    Shorthand {
        name,
        kind,
        longhands,
    }
}

const SIDE_TOP: &[(&str, &str)] = &[
    ("border-top-width", "medium"),
    ("border-top-style", "none"),
    ("border-top-color", "currentcolor"),
];
const SIDE_RIGHT: &[(&str, &str)] = &[
    ("border-right-width", "medium"),
    ("border-right-style", "none"),
    ("border-right-color", "currentcolor"),
];
const SIDE_BOTTOM: &[(&str, &str)] = &[
    ("border-bottom-width", "medium"),
    ("border-bottom-style", "none"),
    ("border-bottom-color", "currentcolor"),
];
const SIDE_LEFT: &[(&str, &str)] = &[
    ("border-left-width", "medium"),
    ("border-left-style", "none"),
    ("border-left-color", "currentcolor"),
];

/// The shorthands with an expansion, longest first within a family so
/// that serialization prefers the shortest form (`border` before
/// `border-top`).
pub static SHORTHANDS: &[Shorthand] = &[
    sh(
        "border",
        Kind::Border,
        &[
            ("border-top-width", "medium"),
            ("border-right-width", "medium"),
            ("border-bottom-width", "medium"),
            ("border-left-width", "medium"),
            ("border-top-style", "none"),
            ("border-right-style", "none"),
            ("border-bottom-style", "none"),
            ("border-left-style", "none"),
            ("border-top-color", "currentcolor"),
            ("border-right-color", "currentcolor"),
            ("border-bottom-color", "currentcolor"),
            ("border-left-color", "currentcolor"),
            ("border-image-source", "none"),
            ("border-image-slice", "100%"),
            ("border-image-width", "1"),
            ("border-image-outset", "0"),
            ("border-image-repeat", "stretch"),
        ],
    ),
    sh(
        "border-width",
        Kind::Box,
        &[
            ("border-top-width", "medium"),
            ("border-right-width", "medium"),
            ("border-bottom-width", "medium"),
            ("border-left-width", "medium"),
        ],
    ),
    sh(
        "border-style",
        Kind::Box,
        &[
            ("border-top-style", "none"),
            ("border-right-style", "none"),
            ("border-bottom-style", "none"),
            ("border-left-style", "none"),
        ],
    ),
    sh(
        "border-color",
        Kind::Box,
        &[
            ("border-top-color", "currentcolor"),
            ("border-right-color", "currentcolor"),
            ("border-bottom-color", "currentcolor"),
            ("border-left-color", "currentcolor"),
        ],
    ),
    sh("border-top", Kind::Side, SIDE_TOP),
    sh("border-right", Kind::Side, SIDE_RIGHT),
    sh("border-bottom", Kind::Side, SIDE_BOTTOM),
    sh("border-left", Kind::Side, SIDE_LEFT),
    sh(
        "border-block",
        Kind::SidePair,
        &[
            ("border-block-start-width", "medium"),
            ("border-block-start-style", "none"),
            ("border-block-start-color", "currentcolor"),
            ("border-block-end-width", "medium"),
            ("border-block-end-style", "none"),
            ("border-block-end-color", "currentcolor"),
        ],
    ),
    sh(
        "border-inline",
        Kind::SidePair,
        &[
            ("border-inline-start-width", "medium"),
            ("border-inline-start-style", "none"),
            ("border-inline-start-color", "currentcolor"),
            ("border-inline-end-width", "medium"),
            ("border-inline-end-style", "none"),
            ("border-inline-end-color", "currentcolor"),
        ],
    ),
    sh(
        "border-block-start",
        Kind::Side,
        &[
            ("border-block-start-width", "medium"),
            ("border-block-start-style", "none"),
            ("border-block-start-color", "currentcolor"),
        ],
    ),
    sh(
        "border-block-end",
        Kind::Side,
        &[
            ("border-block-end-width", "medium"),
            ("border-block-end-style", "none"),
            ("border-block-end-color", "currentcolor"),
        ],
    ),
    sh(
        "border-inline-start",
        Kind::Side,
        &[
            ("border-inline-start-width", "medium"),
            ("border-inline-start-style", "none"),
            ("border-inline-start-color", "currentcolor"),
        ],
    ),
    sh(
        "border-inline-end",
        Kind::Side,
        &[
            ("border-inline-end-width", "medium"),
            ("border-inline-end-style", "none"),
            ("border-inline-end-color", "currentcolor"),
        ],
    ),
    sh(
        "border-block-width",
        Kind::Pair,
        &[
            ("border-block-start-width", "medium"),
            ("border-block-end-width", "medium"),
        ],
    ),
    sh(
        "border-block-style",
        Kind::Pair,
        &[
            ("border-block-start-style", "none"),
            ("border-block-end-style", "none"),
        ],
    ),
    sh(
        "border-block-color",
        Kind::Pair,
        &[
            ("border-block-start-color", "currentcolor"),
            ("border-block-end-color", "currentcolor"),
        ],
    ),
    sh(
        "border-inline-width",
        Kind::Pair,
        &[
            ("border-inline-start-width", "medium"),
            ("border-inline-end-width", "medium"),
        ],
    ),
    sh(
        "border-inline-style",
        Kind::Pair,
        &[
            ("border-inline-start-style", "none"),
            ("border-inline-end-style", "none"),
        ],
    ),
    sh(
        "border-inline-color",
        Kind::Pair,
        &[
            ("border-inline-start-color", "currentcolor"),
            ("border-inline-end-color", "currentcolor"),
        ],
    ),
    sh(
        "border-radius",
        Kind::Radius,
        &[
            ("border-top-left-radius", "0"),
            ("border-top-right-radius", "0"),
            ("border-bottom-right-radius", "0"),
            ("border-bottom-left-radius", "0"),
        ],
    ),
    sh(
        "margin",
        Kind::Box,
        &[
            ("margin-top", "0"),
            ("margin-right", "0"),
            ("margin-bottom", "0"),
            ("margin-left", "0"),
        ],
    ),
    sh(
        "padding",
        Kind::Box,
        &[
            ("padding-top", "0"),
            ("padding-right", "0"),
            ("padding-bottom", "0"),
            ("padding-left", "0"),
        ],
    ),
    sh(
        "inset",
        Kind::Box,
        &[
            ("top", "auto"),
            ("right", "auto"),
            ("bottom", "auto"),
            ("left", "auto"),
        ],
    ),
    sh(
        "scroll-margin",
        Kind::Box,
        &[
            ("scroll-margin-top", "0"),
            ("scroll-margin-right", "0"),
            ("scroll-margin-bottom", "0"),
            ("scroll-margin-left", "0"),
        ],
    ),
    sh(
        "scroll-padding",
        Kind::Box,
        &[
            ("scroll-padding-top", "auto"),
            ("scroll-padding-right", "auto"),
            ("scroll-padding-bottom", "auto"),
            ("scroll-padding-left", "auto"),
        ],
    ),
    sh(
        "margin-block",
        Kind::Pair,
        &[("margin-block-start", "0"), ("margin-block-end", "0")],
    ),
    sh(
        "margin-inline",
        Kind::Pair,
        &[("margin-inline-start", "0"), ("margin-inline-end", "0")],
    ),
    sh(
        "padding-block",
        Kind::Pair,
        &[("padding-block-start", "0"), ("padding-block-end", "0")],
    ),
    sh(
        "padding-inline",
        Kind::Pair,
        &[("padding-inline-start", "0"), ("padding-inline-end", "0")],
    ),
    sh(
        "inset-block",
        Kind::Pair,
        &[("inset-block-start", "auto"), ("inset-block-end", "auto")],
    ),
    sh(
        "inset-inline",
        Kind::Pair,
        &[("inset-inline-start", "auto"), ("inset-inline-end", "auto")],
    ),
    sh(
        "scroll-margin-block",
        Kind::Pair,
        &[
            ("scroll-margin-block-start", "0"),
            ("scroll-margin-block-end", "0"),
        ],
    ),
    sh(
        "scroll-margin-inline",
        Kind::Pair,
        &[
            ("scroll-margin-inline-start", "0"),
            ("scroll-margin-inline-end", "0"),
        ],
    ),
    sh(
        "scroll-padding-block",
        Kind::Pair,
        &[
            ("scroll-padding-block-start", "auto"),
            ("scroll-padding-block-end", "auto"),
        ],
    ),
    sh(
        "scroll-padding-inline",
        Kind::Pair,
        &[
            ("scroll-padding-inline-start", "auto"),
            ("scroll-padding-inline-end", "auto"),
        ],
    ),
    sh(
        "overflow",
        Kind::Pair,
        &[("overflow-x", "visible"), ("overflow-y", "visible")],
    ),
    sh(
        "overscroll-behavior",
        Kind::Pair,
        &[
            ("overscroll-behavior-x", "auto"),
            ("overscroll-behavior-y", "auto"),
        ],
    ),
    sh(
        "gap",
        Kind::Pair,
        &[("row-gap", "normal"), ("column-gap", "normal")],
    ),
    sh(
        "place-content",
        Kind::Pair,
        &[("align-content", "normal"), ("justify-content", "normal")],
    ),
    sh(
        "place-items",
        Kind::Pair,
        &[("align-items", "normal"), ("justify-items", "legacy")],
    ),
    sh(
        "place-self",
        Kind::Pair,
        &[("align-self", "auto"), ("justify-self", "auto")],
    ),
    sh(
        "outline",
        Kind::Side,
        &[
            ("outline-width", "medium"),
            ("outline-style", "none"),
            ("outline-color", "auto"),
        ],
    ),
    sh(
        "column-rule",
        Kind::Side,
        &[
            ("column-rule-width", "medium"),
            ("column-rule-style", "none"),
            ("column-rule-color", "currentcolor"),
        ],
    ),
    sh(
        "flex",
        Kind::Flex,
        &[
            ("flex-grow", "0"),
            ("flex-shrink", "1"),
            ("flex-basis", "auto"),
        ],
    ),
    sh(
        "flex-flow",
        Kind::FlexFlow,
        &[("flex-direction", "row"), ("flex-wrap", "nowrap")],
    ),
    sh(
        "list-style",
        Kind::ListStyle,
        &[
            ("list-style-position", "outside"),
            ("list-style-image", "none"),
            ("list-style-type", "disc"),
        ],
    ),
    sh(
        "text-decoration",
        Kind::TextDecoration,
        &[
            ("text-decoration-line", "none"),
            ("text-decoration-thickness", "auto"),
            ("text-decoration-style", "solid"),
            ("text-decoration-color", "currentcolor"),
        ],
    ),
    sh(
        "columns",
        Kind::Columns,
        &[("column-width", "auto"), ("column-count", "auto")],
    ),
    sh(
        "grid-row",
        Kind::GridLine,
        &[("grid-row-start", "auto"), ("grid-row-end", "auto")],
    ),
    sh(
        "grid-column",
        Kind::GridLine,
        &[("grid-column-start", "auto"), ("grid-column-end", "auto")],
    ),
    sh(
        "grid-area",
        Kind::GridArea,
        &[
            ("grid-row-start", "auto"),
            ("grid-column-start", "auto"),
            ("grid-row-end", "auto"),
            ("grid-column-end", "auto"),
        ],
    ),
];

/// Legacy names that parse as another property (CSS Cascade's "legacy
/// name aliases").
pub const ALIASES: &[(&str, &str)] = &[
    ("word-wrap", "overflow-wrap"),
    ("grid-gap", "gap"),
    ("grid-row-gap", "row-gap"),
    ("grid-column-gap", "column-gap"),
    ("page-break-after", "break-after"),
    ("page-break-before", "break-before"),
    ("page-break-inside", "break-inside"),
];

/// The descriptors of `@font-face`.
pub const FONT_FACE_DESCRIPTORS: &[&str] = &[
    "ascent-override",
    "descent-override",
    "font-display",
    "font-family",
    "font-feature-settings",
    "font-language-override",
    "font-named-instance",
    "font-stretch",
    "font-style",
    "font-variation-settings",
    "font-weight",
    "font-width",
    "line-gap-override",
    "size-adjust",
    "src",
    "subscript-position-override",
    "subscript-size-override",
    "superscript-position-override",
    "superscript-size-override",
    "unicode-range",
];

/// The descriptors of `@page` besides the properties.
pub const PAGE_DESCRIPTORS: &[&str] = &["size", "page-orientation", "marks", "bleed"];

/// The descriptors of `@property`.
pub const PROPERTY_DESCRIPTORS: &[&str] = &["syntax", "inherits", "initial-value"];

/// The shorthand named `name`, if it has an expansion here.
pub fn shorthand(name: &str) -> Option<&'static Shorthand> {
    SHORTHANDS.iter().find(|s| s.name == name)
}

/// The shorthands with an expansion that contain `longhand`, in the
/// order of [`SHORTHANDS`].
pub fn shorthands_of(longhand: &str) -> impl Iterator<Item = &'static Shorthand> + '_ {
    SHORTHANDS
        .iter()
        .filter(move |s| s.longhands.iter().any(|(l, _)| *l == longhand))
}

/// The initial value of a longhand of some shorthand.
pub fn initial_value(longhand: &str) -> Option<&'static str> {
    SHORTHANDS
        .iter()
        .flat_map(|s| s.longhands.iter())
        .find(|(l, _)| *l == longhand)
        .map(|(_, v)| *v)
}

/// The property `name` (lowercase) stands for after resolving legacy
/// aliases, if it is a supported property.
pub fn canonical(name: &str) -> Option<&'static str> {
    if let Some((_, to)) = ALIASES.iter().find(|(a, _)| *a == name) {
        return Some(to);
    }
    if let Some(s) = shorthand(name) {
        return Some(s.name);
    }
    LONGHANDS.iter().find(|p| **p == name).copied()
}

/// True for a supported CSS property (not custom).
pub fn is_supported(name: &str) -> bool {
    canonical(name).is_some()
}

/// Every supported property name, shorthands included, aliases excluded.
pub fn all_names() -> impl Iterator<Item = &'static str> {
    LONGHANDS
        .iter()
        .copied()
        .chain(SHORTHANDS.iter().map(|s| s.name))
}

/// The CSS-wide keywords.
pub fn is_css_wide_keyword(s: &str) -> bool {
    ["initial", "inherit", "unset", "revert", "revert-layer"]
        .iter()
        .any(|k| s.eq_ignore_ascii_case(k))
}

/// The camel-cased attribute of a property (CSSOM §6.7.1): each `-`
/// followed by a letter becomes the uppercase letter.
pub fn camel_case(property: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in property.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// The property an IDL attribute of `CSSStyleProperties` (or of the
/// descriptor interfaces) stands for, from the member name of its
/// generated operation: `marginTop`, `margin_top` (the dashed attribute
/// `margin-top`) or `cssFloat`.
pub fn property_of_attribute(member: &str) -> String {
    if member == "cssFloat" {
        return "float".to_string();
    }
    if member.contains('_') {
        return member.replace('_', "-");
    }
    let mut out = String::new();
    for c in member.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_through_attributes() {
        for name in all_names() {
            assert_eq!(property_of_attribute(&camel_case(name)), name, "{name}");
            assert!(is_supported(name));
        }
        assert_eq!(canonical("word-wrap"), Some("overflow-wrap"));
        assert_eq!(property_of_attribute("cssFloat"), "float");
        assert_eq!(
            property_of_attribute("border_top_width"),
            "border-top-width"
        );
    }

    #[test]
    fn longhands_of_shorthands_are_longhands() {
        for s in SHORTHANDS {
            for (l, _) in s.longhands {
                assert!(LONGHANDS.contains(l), "{} of {}", l, s.name);
            }
        }
    }

    /// The IDL attributes of `CSSStyleProperties` in
    /// `idl/cssom-properties.idl` are the camel-cased and dashed names of
    /// every property.
    #[test]
    fn the_idl_lists_every_property() {
        let idl = include_str!("../../idl/cssom-properties.idl");
        for name in all_names() {
            let camel = format!(" {};", camel_case(name));
            assert!(idl.contains(&camel), "{name}");
            if name.contains('-') {
                assert!(idl.contains(&format!(" {name};")), "{name}");
            }
        }
    }
}
