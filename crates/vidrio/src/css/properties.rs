//! The CSS properties CSSOM knows by name: the longhands, the shorthands
//! with their longhands and the initial values the expansion of a
//! shorthand gives to the longhands it does not mention, legacy name
//! aliases, the descriptors of `@font-face`, `@page` and `@property`, and
//! the IDL attribute names of `CSSStyleProperties` (CSSOM §6.7.1), and
//! the property database: inheritance, initial value, what each longhand
//! applies to, its computed value rule and its value grammar, the
//! longhands of every shorthand and the logical property groups.

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

// ----- the property database -------------------------------------------------------------

/// What a property applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applies {
    All,
    /// All elements but non-replaced inline boxes, table rows and row
    /// groups (`width`, `height`).
    Boxes,
    /// All elements but internal table elements (`margin`, `padding`).
    NonTableInternal,
    Positioned,
    BlockContainers,
    FlexItems,
    FlexContainers,
    GridContainers,
    GridItems,
    FlexGridItems,
    /// Block containers, multicolumn, flex and grid containers.
    AlignContainers,
    MultiColumn,
    Tables,
    TableCells,
    TableCaptions,
    ListItems,
    InlineAndTableCells,
    Replaced,
    Transformable,
    ScrollContainers,
    Svg,
    SvgShapes,
    SvgText,
}

/// How a property's computed value comes from its specified value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Computed {
    /// Keywords as specified, absolute dimensions in canonical units,
    /// colors and images computed, percentages kept.
    Generic,
    /// An absolute length.
    Length,
    /// An absolute length, a percentage or a `calc()` of both.
    LengthPercentage,
    Number,
    /// An integer (math functions rounded).
    Integer,
    /// A computed color (`currentcolor` kept, except on `color`).
    Color,
    /// `<number> | <percentage>` as a number clamped to [0, 1].
    AlphaValue,
    /// `thin`, `medium` and `thick` as lengths (zero when the matching
    /// style is `none` or `hidden`, which the cascade applies).
    LineWidth,
    /// Images: URLs and gradients with computed colors and lengths.
    Image,
    /// An absolute length from keywords, lengths and percentages of the
    /// parent's font size.
    FontSize,
    /// A number from keywords and relative weights.
    FontWeight,
    /// A percentage from keywords.
    FontStretch,
    /// `normal`, a number or an absolute length.
    LineHeight,
}

/// A longhand: name, inheritance, initial value, what it applies to, its
/// computed value rule and its grammar in the value definition syntax
/// (`None` keeps a well-formed value as written; see LIMITATIONS.md).
pub struct Property {
    pub name: &'static str,
    pub inherited: bool,
    pub initial: &'static str,
    pub applies: Applies,
    pub computed: Computed,
    pub grammar: Option<&'static str>,
}

const fn p(
    name: &'static str,
    inherited: bool,
    initial: &'static str,
    applies: Applies,
    computed: Computed,
    grammar: &'static str,
) -> Property {
    Property {
        name,
        inherited,
        initial,
        applies,
        computed,
        grammar: if grammar.is_empty() {
            None
        } else {
            Some(grammar)
        },
    }
}

const Y: bool = true;
const N: bool = false;
use Applies as A;
use Computed as Cv;

const COLOR: &str = "<color>";
const LINE_STYLE: &str = "<line-style>";
const LINE_WIDTH: &str = "<line-width>";
const RADIUS: &str = "<length-percentage [0,∞]>{1,2}";
const INSET: &str = "auto | <length-percentage>";
const MARGIN: &str = "<length-percentage> | auto";
const PADDING: &str = "<length-percentage [0,∞]>";
const SIZE: &str = "<size>";
const MAX_SIZE: &str = "<max-size>";
const ALPHA: &str = "<number> | <percentage>";
const OVERFLOW: &str = "visible | hidden | clip | scroll | auto";
const OVERSCROLL: &str = "contain | none | auto";
const SCROLL_PADDING: &str = "auto | <length-percentage [0,∞]>";
const INTRINSIC: &str = "auto? [ none | <length [0,∞]> ]";
const BREAK: &str = "auto | avoid | always | all | avoid-page | page | left | right | recto | verso \
                     | avoid-column | column | avoid-region | region";
const SYNTHESIS: &str = "auto | none";
const GRID_LINE: &str = "<grid-line>";
const TEMPLATE: &str = "none | <track-list> | <auto-track-list> | subgrid <line-name-list>?";
const COUNTER: &str = "[ <custom-ident> <integer>? ]+ | none";
const MARKER: &str = "none | <url>";
const EASING: &str = "<easing-function>#";
const DISPLAY: &str = "[ [ block | inline | run-in ] || [ flow | flow-root | table | flex | grid | ruby | math ] ] \
     | [ [ block | inline | run-in ]? && [ flow | flow-root ]? && list-item ] \
     | table-row-group | table-header-group | table-footer-group | table-row | table-cell \
     | table-column-group | table-column | table-caption | ruby-base | ruby-text \
     | ruby-base-container | ruby-text-container | contents | none | inline-block | inline-table \
     | inline-flex | inline-grid | -webkit-box | -webkit-inline-box";
const CURSOR: &str = "[ <url> [ <number> <number> ]? , ]* [ auto | default | none | context-menu \
     | help | pointer | progress | wait | cell | crosshair | text | vertical-text | alias | copy \
     | move | no-drop | not-allowed | grab | grabbing | e-resize | n-resize | ne-resize | nw-resize \
     | s-resize | se-resize | sw-resize | w-resize | ew-resize | ns-resize | nesw-resize \
     | nwse-resize | col-resize | row-resize | all-scroll | zoom-in | zoom-out ]";

/// The longhands with their definitions, in alphabetical order.
pub static PROPERTIES: &[Property] = &[
    p(
        "accent-color",
        Y,
        "auto",
        A::All,
        Cv::Color,
        "auto | <color>",
    ),
    p(
        "align-content",
        N,
        "normal",
        A::AlignContainers,
        Cv::Generic,
        "normal | <baseline-position> | <content-distribution> | <overflow-position>? <content-position>",
    ),
    p(
        "align-items",
        N,
        "normal",
        A::All,
        Cv::Generic,
        "normal | stretch | <baseline-position> | <overflow-position>? <self-position> | anchor-center",
    ),
    p(
        "align-self",
        N,
        "auto",
        A::FlexGridItems,
        Cv::Generic,
        "auto | normal | stretch | <baseline-position> | <overflow-position>? <self-position> | anchor-center",
    ),
    p(
        "alignment-baseline",
        N,
        "baseline",
        A::InlineAndTableCells,
        Cv::Generic,
        "baseline | text-bottom | alphabetic | ideographic | middle | central | mathematical | text-top",
    ),
    p(
        "anchor-name",
        N,
        "none",
        A::Boxes,
        Cv::Generic,
        "none | <dashed-ident>#",
    ),
    p(
        "animation-composition",
        N,
        "replace",
        A::All,
        Cv::Generic,
        "[ replace | add | accumulate ]#",
    ),
    p("animation-delay", N, "0s", A::All, Cv::Generic, "<time>#"),
    p(
        "animation-direction",
        N,
        "normal",
        A::All,
        Cv::Generic,
        "[ normal | reverse | alternate | alternate-reverse ]#",
    ),
    p(
        "animation-duration",
        N,
        "auto",
        A::All,
        Cv::Generic,
        "[ auto | <time [0,∞]> ]#",
    ),
    p(
        "animation-fill-mode",
        N,
        "none",
        A::All,
        Cv::Generic,
        "[ none | forwards | backwards | both ]#",
    ),
    p(
        "animation-iteration-count",
        N,
        "1",
        A::All,
        Cv::Generic,
        "[ infinite | <number [0,∞]> ]#",
    ),
    p(
        "animation-name",
        N,
        "none",
        A::All,
        Cv::Generic,
        "[ none | <custom-ident> | <string> ]#",
    ),
    p(
        "animation-play-state",
        N,
        "running",
        A::All,
        Cv::Generic,
        "[ running | paused ]#",
    ),
    p(
        "animation-timing-function",
        N,
        "ease",
        A::All,
        Cv::Generic,
        EASING,
    ),
    p(
        "appearance",
        N,
        "none",
        A::All,
        Cv::Generic,
        "none | auto | base | base-select | menulist-button | textfield | button | checkbox | listbox \
       | menulist | meter | progress-bar | radio | searchfield | textarea | square-button \
       | push-button | slider-horizontal",
    ),
    p(
        "aspect-ratio",
        N,
        "auto",
        A::Boxes,
        Cv::Generic,
        "auto || <ratio>",
    ),
    p("backdrop-filter", N, "none", A::All, Cv::Generic, ""),
    p(
        "backface-visibility",
        N,
        "visible",
        A::Transformable,
        Cv::Generic,
        "visible | hidden",
    ),
    p(
        "background-attachment",
        N,
        "scroll",
        A::All,
        Cv::Generic,
        "[ scroll | fixed | local ]#",
    ),
    p(
        "background-blend-mode",
        N,
        "normal",
        A::All,
        Cv::Generic,
        "<blend-mode>#",
    ),
    p(
        "background-clip",
        N,
        "border-box",
        A::All,
        Cv::Generic,
        "[ border-box | padding-box | content-box | text | border-area ]#",
    ),
    p(
        "background-color",
        N,
        "transparent",
        A::All,
        Cv::Color,
        COLOR,
    ),
    p(
        "background-image",
        N,
        "none",
        A::All,
        Cv::Image,
        "[ none | <image> ]#",
    ),
    p(
        "background-origin",
        N,
        "padding-box",
        A::All,
        Cv::Generic,
        "<box>#",
    ),
    p(
        "background-position-x",
        N,
        "0%",
        A::All,
        Cv::LengthPercentage,
        "[ center | [ left | right | x-start | x-end ] <length-percentage>? | <length-percentage> ]#",
    ),
    p(
        "background-position-y",
        N,
        "0%",
        A::All,
        Cv::LengthPercentage,
        "[ center | [ top | bottom | y-start | y-end ] <length-percentage>? | <length-percentage> ]#",
    ),
    p(
        "background-repeat",
        N,
        "repeat",
        A::All,
        Cv::Generic,
        "<repeat-style>#",
    ),
    p(
        "background-size",
        N,
        "auto",
        A::All,
        Cv::LengthPercentage,
        "<bg-size>#",
    ),
    p(
        "baseline-shift",
        N,
        "0",
        A::InlineAndTableCells,
        Cv::LengthPercentage,
        "<length-percentage> | sub | super | top | center | bottom",
    ),
    p(
        "baseline-source",
        N,
        "auto",
        A::InlineAndTableCells,
        Cv::Generic,
        "auto | first | last",
    ),
    p(
        "block-size",
        N,
        "auto",
        A::Boxes,
        Cv::LengthPercentage,
        SIZE,
    ),
    p(
        "border-block-end-color",
        N,
        "currentcolor",
        A::All,
        Cv::Color,
        COLOR,
    ),
    p(
        "border-block-end-style",
        N,
        "none",
        A::All,
        Cv::Generic,
        LINE_STYLE,
    ),
    p(
        "border-block-end-width",
        N,
        "medium",
        A::All,
        Cv::LineWidth,
        LINE_WIDTH,
    ),
    p(
        "border-block-start-color",
        N,
        "currentcolor",
        A::All,
        Cv::Color,
        COLOR,
    ),
    p(
        "border-block-start-style",
        N,
        "none",
        A::All,
        Cv::Generic,
        LINE_STYLE,
    ),
    p(
        "border-block-start-width",
        N,
        "medium",
        A::All,
        Cv::LineWidth,
        LINE_WIDTH,
    ),
    p(
        "border-bottom-color",
        N,
        "currentcolor",
        A::All,
        Cv::Color,
        COLOR,
    ),
    p(
        "border-bottom-left-radius",
        N,
        "0",
        A::All,
        Cv::LengthPercentage,
        RADIUS,
    ),
    p(
        "border-bottom-right-radius",
        N,
        "0",
        A::All,
        Cv::LengthPercentage,
        RADIUS,
    ),
    p(
        "border-bottom-style",
        N,
        "none",
        A::All,
        Cv::Generic,
        LINE_STYLE,
    ),
    p(
        "border-bottom-width",
        N,
        "medium",
        A::All,
        Cv::LineWidth,
        LINE_WIDTH,
    ),
    p(
        "border-collapse",
        Y,
        "separate",
        A::Tables,
        Cv::Generic,
        "separate | collapse",
    ),
    p(
        "border-end-end-radius",
        N,
        "0",
        A::All,
        Cv::LengthPercentage,
        RADIUS,
    ),
    p(
        "border-end-start-radius",
        N,
        "0",
        A::All,
        Cv::LengthPercentage,
        RADIUS,
    ),
    p(
        "border-image-outset",
        N,
        "0",
        A::All,
        Cv::Generic,
        "[ <number [0,∞]> | <length [0,∞]> ]{1,4}",
    ),
    p(
        "border-image-repeat",
        N,
        "stretch",
        A::All,
        Cv::Generic,
        "[ stretch | repeat | round | space ]{1,2}",
    ),
    p(
        "border-image-slice",
        N,
        "100%",
        A::All,
        Cv::Generic,
        "[ <number [0,∞]> | <percentage [0,∞]> ]{1,4} && fill?",
    ),
    p(
        "border-image-source",
        N,
        "none",
        A::All,
        Cv::Image,
        "none | <image>",
    ),
    p(
        "border-image-width",
        N,
        "1",
        A::All,
        Cv::Generic,
        "[ <number [0,∞]> | <length-percentage [0,∞]> | auto ]{1,4}",
    ),
    p(
        "border-inline-end-color",
        N,
        "currentcolor",
        A::All,
        Cv::Color,
        COLOR,
    ),
    p(
        "border-inline-end-style",
        N,
        "none",
        A::All,
        Cv::Generic,
        LINE_STYLE,
    ),
    p(
        "border-inline-end-width",
        N,
        "medium",
        A::All,
        Cv::LineWidth,
        LINE_WIDTH,
    ),
    p(
        "border-inline-start-color",
        N,
        "currentcolor",
        A::All,
        Cv::Color,
        COLOR,
    ),
    p(
        "border-inline-start-style",
        N,
        "none",
        A::All,
        Cv::Generic,
        LINE_STYLE,
    ),
    p(
        "border-inline-start-width",
        N,
        "medium",
        A::All,
        Cv::LineWidth,
        LINE_WIDTH,
    ),
    p(
        "border-left-color",
        N,
        "currentcolor",
        A::All,
        Cv::Color,
        COLOR,
    ),
    p(
        "border-left-style",
        N,
        "none",
        A::All,
        Cv::Generic,
        LINE_STYLE,
    ),
    p(
        "border-left-width",
        N,
        "medium",
        A::All,
        Cv::LineWidth,
        LINE_WIDTH,
    ),
    p(
        "border-right-color",
        N,
        "currentcolor",
        A::All,
        Cv::Color,
        COLOR,
    ),
    p(
        "border-right-style",
        N,
        "none",
        A::All,
        Cv::Generic,
        LINE_STYLE,
    ),
    p(
        "border-right-width",
        N,
        "medium",
        A::All,
        Cv::LineWidth,
        LINE_WIDTH,
    ),
    p(
        "border-spacing",
        Y,
        "0px 0px",
        A::Tables,
        Cv::Length,
        "<length [0,∞]>{1,2}",
    ),
    p(
        "border-start-end-radius",
        N,
        "0",
        A::All,
        Cv::LengthPercentage,
        RADIUS,
    ),
    p(
        "border-start-start-radius",
        N,
        "0",
        A::All,
        Cv::LengthPercentage,
        RADIUS,
    ),
    p(
        "border-top-color",
        N,
        "currentcolor",
        A::All,
        Cv::Color,
        COLOR,
    ),
    p(
        "border-top-left-radius",
        N,
        "0",
        A::All,
        Cv::LengthPercentage,
        RADIUS,
    ),
    p(
        "border-top-right-radius",
        N,
        "0",
        A::All,
        Cv::LengthPercentage,
        RADIUS,
    ),
    p(
        "border-top-style",
        N,
        "none",
        A::All,
        Cv::Generic,
        LINE_STYLE,
    ),
    p(
        "border-top-width",
        N,
        "medium",
        A::All,
        Cv::LineWidth,
        LINE_WIDTH,
    ),
    p(
        "bottom",
        N,
        "auto",
        A::Positioned,
        Cv::LengthPercentage,
        INSET,
    ),
    p(
        "box-decoration-break",
        N,
        "slice",
        A::All,
        Cv::Generic,
        "slice | clone",
    ),
    p(
        "box-shadow",
        N,
        "none",
        A::All,
        Cv::Generic,
        "none | <shadow>#",
    ),
    p(
        "box-sizing",
        N,
        "content-box",
        A::Boxes,
        Cv::Generic,
        "content-box | border-box",
    ),
    p(
        "break-after",
        N,
        "auto",
        A::BlockContainers,
        Cv::Generic,
        BREAK,
    ),
    p(
        "break-before",
        N,
        "auto",
        A::BlockContainers,
        Cv::Generic,
        BREAK,
    ),
    p(
        "break-inside",
        N,
        "auto",
        A::All,
        Cv::Generic,
        "auto | avoid | avoid-page | avoid-column | avoid-region",
    ),
    p(
        "caption-side",
        Y,
        "top",
        A::TableCaptions,
        Cv::Generic,
        "top | bottom",
    ),
    p(
        "caret-color",
        Y,
        "auto",
        A::All,
        Cv::Color,
        "auto | <color>",
    ),
    p(
        "clear",
        N,
        "none",
        A::BlockContainers,
        Cv::Generic,
        "inline-start | inline-end | block-start | block-end | left | right | top | bottom \
       | both-inline | both-block | both | none",
    ),
    p("clip", N, "auto", A::Positioned, Cv::Generic, ""),
    p("clip-path", N, "none", A::All, Cv::Generic, ""),
    p(
        "clip-rule",
        Y,
        "nonzero",
        A::Svg,
        Cv::Generic,
        "nonzero | evenodd",
    ),
    p("color", Y, "canvastext", A::All, Cv::Color, COLOR),
    p(
        "color-interpolation",
        Y,
        "srgb",
        A::Svg,
        Cv::Generic,
        "auto | sRGB | linearRGB",
    ),
    p(
        "color-interpolation-filters",
        Y,
        "linearrgb",
        A::Svg,
        Cv::Generic,
        "auto | sRGB | linearRGB",
    ),
    p(
        "color-scheme",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | [ light | dark | <custom-ident> ]+ && only?",
    ),
    p(
        "column-count",
        N,
        "auto",
        A::MultiColumn,
        Cv::Generic,
        "auto | <integer [1,∞]>",
    ),
    p(
        "column-fill",
        N,
        "balance",
        A::MultiColumn,
        Cv::Generic,
        "auto | balance | balance-all",
    ),
    p(
        "column-gap",
        N,
        "normal",
        A::AlignContainers,
        Cv::LengthPercentage,
        "normal | <length-percentage [0,∞]>",
    ),
    p(
        "column-rule-color",
        N,
        "currentcolor",
        A::MultiColumn,
        Cv::Color,
        COLOR,
    ),
    p(
        "column-rule-style",
        N,
        "none",
        A::MultiColumn,
        Cv::Generic,
        LINE_STYLE,
    ),
    p(
        "column-rule-width",
        N,
        "medium",
        A::MultiColumn,
        Cv::LineWidth,
        LINE_WIDTH,
    ),
    p(
        "column-span",
        N,
        "none",
        A::BlockContainers,
        Cv::Generic,
        "none | all",
    ),
    p(
        "column-width",
        N,
        "auto",
        A::MultiColumn,
        Cv::Length,
        "auto | <length [0,∞]>",
    ),
    p(
        "contain",
        N,
        "none",
        A::All,
        Cv::Generic,
        "none | strict | content | [ [ size | inline-size ] || layout || style || paint ]",
    ),
    p(
        "contain-intrinsic-block-size",
        N,
        "none",
        A::Boxes,
        Cv::Length,
        INTRINSIC,
    ),
    p(
        "contain-intrinsic-height",
        N,
        "none",
        A::Boxes,
        Cv::Length,
        INTRINSIC,
    ),
    p(
        "contain-intrinsic-inline-size",
        N,
        "none",
        A::Boxes,
        Cv::Length,
        INTRINSIC,
    ),
    p(
        "contain-intrinsic-width",
        N,
        "none",
        A::Boxes,
        Cv::Length,
        INTRINSIC,
    ),
    p(
        "container-name",
        N,
        "none",
        A::All,
        Cv::Generic,
        "none | <custom-ident>+",
    ),
    p(
        "container-type",
        N,
        "normal",
        A::All,
        Cv::Generic,
        "normal | [ size | inline-size ] || scroll-state",
    ),
    p("content", N, "normal", A::All, Cv::Generic, ""),
    p(
        "content-visibility",
        N,
        "visible",
        A::All,
        Cv::Generic,
        "visible | auto | hidden",
    ),
    p("counter-increment", N, "none", A::All, Cv::Integer, COUNTER),
    p(
        "counter-reset",
        N,
        "none",
        A::All,
        Cv::Integer,
        "[ [ <custom-ident> | reversed( <custom-ident> ) ] <integer>? ]+ | none",
    ),
    p("counter-set", N, "none", A::All, Cv::Integer, COUNTER),
    p("cursor", Y, "auto", A::All, Cv::Generic, CURSOR),
    p(
        "cx",
        N,
        "0",
        A::Svg,
        Cv::LengthPercentage,
        "<length-percentage>",
    ),
    p(
        "cy",
        N,
        "0",
        A::Svg,
        Cv::LengthPercentage,
        "<length-percentage>",
    ),
    p("d", N, "none", A::Svg, Cv::Generic, ""),
    p("direction", Y, "ltr", A::All, Cv::Generic, "ltr | rtl"),
    p("display", N, "inline", A::All, Cv::Generic, DISPLAY),
    p(
        "dominant-baseline",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | text-bottom | alphabetic | ideographic | middle | central | mathematical | hanging | text-top",
    ),
    p(
        "empty-cells",
        Y,
        "show",
        A::TableCells,
        Cv::Generic,
        "show | hide",
    ),
    p("fill", Y, "black", A::Svg, Cv::Color, "<paint>"),
    p("fill-opacity", Y, "1", A::Svg, Cv::AlphaValue, ALPHA),
    p(
        "fill-rule",
        Y,
        "nonzero",
        A::Svg,
        Cv::Generic,
        "nonzero | evenodd",
    ),
    p("filter", N, "none", A::All, Cv::Generic, ""),
    p(
        "flex-basis",
        N,
        "auto",
        A::FlexItems,
        Cv::LengthPercentage,
        "content | auto | <length-percentage [0,∞]> | min-content | max-content | fit-content",
    ),
    p(
        "flex-direction",
        N,
        "row",
        A::FlexContainers,
        Cv::Generic,
        "row | row-reverse | column | column-reverse",
    ),
    p(
        "flex-grow",
        N,
        "0",
        A::FlexItems,
        Cv::Number,
        "<number [0,∞]>",
    ),
    p(
        "flex-shrink",
        N,
        "1",
        A::FlexItems,
        Cv::Number,
        "<number [0,∞]>",
    ),
    p(
        "flex-wrap",
        N,
        "nowrap",
        A::FlexContainers,
        Cv::Generic,
        "nowrap | wrap | wrap-reverse",
    ),
    p(
        "float",
        N,
        "none",
        A::All,
        Cv::Generic,
        "left | right | none | inline-start | inline-end",
    ),
    p("flood-color", N, "black", A::Svg, Cv::Color, COLOR),
    p("flood-opacity", N, "1", A::Svg, Cv::AlphaValue, ALPHA),
    p(
        "font-family",
        Y,
        "serif",
        A::All,
        Cv::Generic,
        "[ <string> | <generic-family> | <custom-ident>+ ]#",
    ),
    p(
        "font-feature-settings",
        Y,
        "normal",
        A::All,
        Cv::Integer,
        "normal | [ <string> [ <integer [0,∞]> | on | off ]? ]#",
    ),
    p(
        "font-kerning",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | normal | none",
    ),
    p(
        "font-language-override",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | <string>",
    ),
    p(
        "font-optical-sizing",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | none",
    ),
    p(
        "font-palette",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | light | dark | <dashed-ident>",
    ),
    p(
        "font-size",
        Y,
        "medium",
        A::All,
        Cv::FontSize,
        "<absolute-size> | larger | smaller | <length-percentage [0,∞]> | math",
    ),
    p(
        "font-size-adjust",
        Y,
        "none",
        A::All,
        Cv::Generic,
        "none | [ ex-height | cap-height | ch-width | ic-width | ic-height ]? [ from-font | <number [0,∞]> ]",
    ),
    p(
        "font-stretch",
        Y,
        "normal",
        A::All,
        Cv::FontStretch,
        "normal | <percentage [0,∞]> | ultra-condensed | extra-condensed | condensed | semi-condensed \
       | semi-expanded | expanded | extra-expanded | ultra-expanded",
    ),
    p(
        "font-style",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | italic | oblique <angle [-90,90]>?",
    ),
    p(
        "font-synthesis-small-caps",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        SYNTHESIS,
    ),
    p(
        "font-synthesis-style",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        SYNTHESIS,
    ),
    p(
        "font-synthesis-weight",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        SYNTHESIS,
    ),
    p(
        "font-variant-alternates",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "",
    ),
    p(
        "font-variant-caps",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | small-caps | all-small-caps | petite-caps | all-petite-caps | unicase | titling-caps",
    ),
    p(
        "font-variant-east-asian",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | [ [ jis78 | jis83 | jis90 | jis04 | simplified | traditional ] \
       || [ full-width | proportional-width ] || ruby ]",
    ),
    p(
        "font-variant-emoji",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | text | emoji | unicode",
    ),
    p(
        "font-variant-ligatures",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | none | [ [ common-ligatures | no-common-ligatures ] \
       || [ discretionary-ligatures | no-discretionary-ligatures ] \
       || [ historical-ligatures | no-historical-ligatures ] || [ contextual | no-contextual ] ]",
    ),
    p(
        "font-variant-numeric",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | [ [ lining-nums | oldstyle-nums ] || [ proportional-nums | tabular-nums ] \
       || [ diagonal-fractions | stacked-fractions ] || ordinal || slashed-zero ]",
    ),
    p(
        "font-variant-position",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | sub | super",
    ),
    p(
        "font-variation-settings",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | [ <string> <number> ]#",
    ),
    p(
        "font-weight",
        Y,
        "normal",
        A::All,
        Cv::FontWeight,
        "normal | bold | bolder | lighter | <number [1,1000]>",
    ),
    p(
        "forced-color-adjust",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | none | preserve-parent-color",
    ),
    p(
        "grid-auto-columns",
        N,
        "auto",
        A::GridContainers,
        Cv::LengthPercentage,
        "<track-size>+",
    ),
    p(
        "grid-auto-flow",
        N,
        "row",
        A::GridContainers,
        Cv::Generic,
        "[ row | column ] || dense",
    ),
    p(
        "grid-auto-rows",
        N,
        "auto",
        A::GridContainers,
        Cv::LengthPercentage,
        "<track-size>+",
    ),
    p(
        "grid-column-end",
        N,
        "auto",
        A::GridItems,
        Cv::Generic,
        GRID_LINE,
    ),
    p(
        "grid-column-start",
        N,
        "auto",
        A::GridItems,
        Cv::Generic,
        GRID_LINE,
    ),
    p(
        "grid-row-end",
        N,
        "auto",
        A::GridItems,
        Cv::Generic,
        GRID_LINE,
    ),
    p(
        "grid-row-start",
        N,
        "auto",
        A::GridItems,
        Cv::Generic,
        GRID_LINE,
    ),
    p(
        "grid-template-areas",
        N,
        "none",
        A::GridContainers,
        Cv::Generic,
        "none | <string>+",
    ),
    p(
        "grid-template-columns",
        N,
        "none",
        A::GridContainers,
        Cv::LengthPercentage,
        TEMPLATE,
    ),
    p(
        "grid-template-rows",
        N,
        "none",
        A::GridContainers,
        Cv::LengthPercentage,
        TEMPLATE,
    ),
    p(
        "hanging-punctuation",
        Y,
        "none",
        A::All,
        Cv::Generic,
        "none | [ first || [ force-end | allow-end ] || last ]",
    ),
    p("height", N, "auto", A::Boxes, Cv::LengthPercentage, SIZE),
    p(
        "hyphenate-character",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | <string>",
    ),
    p(
        "hyphens",
        Y,
        "manual",
        A::All,
        Cv::Generic,
        "none | manual | auto",
    ),
    p(
        "image-orientation",
        Y,
        "from-image",
        A::All,
        Cv::Generic,
        "from-image | none | [ <angle> || flip ]",
    ),
    p(
        "image-rendering",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | smooth | high-quality | crisp-edges | pixelated | optimizeSpeed | optimizeQuality",
    ),
    p(
        "inline-size",
        N,
        "auto",
        A::Boxes,
        Cv::LengthPercentage,
        SIZE,
    ),
    p(
        "inset-block-end",
        N,
        "auto",
        A::Positioned,
        Cv::LengthPercentage,
        INSET,
    ),
    p(
        "inset-block-start",
        N,
        "auto",
        A::Positioned,
        Cv::LengthPercentage,
        INSET,
    ),
    p(
        "inset-inline-end",
        N,
        "auto",
        A::Positioned,
        Cv::LengthPercentage,
        INSET,
    ),
    p(
        "inset-inline-start",
        N,
        "auto",
        A::Positioned,
        Cv::LengthPercentage,
        INSET,
    ),
    p(
        "isolation",
        N,
        "auto",
        A::All,
        Cv::Generic,
        "auto | isolate",
    ),
    p(
        "justify-content",
        N,
        "normal",
        A::AlignContainers,
        Cv::Generic,
        "normal | <content-distribution> | <overflow-position>? [ <content-position> | left | right ]",
    ),
    p(
        "justify-items",
        N,
        "legacy",
        A::All,
        Cv::Generic,
        "normal | stretch | <baseline-position> | <overflow-position>? [ <self-position> | left | right ] \
       | legacy | legacy && [ left | right | center ] | anchor-center",
    ),
    p(
        "justify-self",
        N,
        "auto",
        A::All,
        Cv::Generic,
        "auto | normal | stretch | <baseline-position> \
       | <overflow-position>? [ <self-position> | left | right ] | anchor-center",
    ),
    p(
        "left",
        N,
        "auto",
        A::Positioned,
        Cv::LengthPercentage,
        INSET,
    ),
    p(
        "letter-spacing",
        Y,
        "normal",
        A::All,
        Cv::LengthPercentage,
        "normal | <length-percentage>",
    ),
    p("lighting-color", N, "white", A::Svg, Cv::Color, COLOR),
    p(
        "line-break",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | loose | normal | strict | anywhere",
    ),
    p(
        "line-height",
        Y,
        "normal",
        A::All,
        Cv::LineHeight,
        "normal | <number [0,∞]> | <length-percentage [0,∞]>",
    ),
    p(
        "list-style-image",
        Y,
        "none",
        A::ListItems,
        Cv::Image,
        "none | <image>",
    ),
    p(
        "list-style-position",
        Y,
        "outside",
        A::ListItems,
        Cv::Generic,
        "inside | outside",
    ),
    p(
        "list-style-type",
        Y,
        "disc",
        A::ListItems,
        Cv::Generic,
        "<string> | none | <custom-ident>",
    ),
    p(
        "margin-block-end",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        MARGIN,
    ),
    p(
        "margin-block-start",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        MARGIN,
    ),
    p(
        "margin-bottom",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        MARGIN,
    ),
    p(
        "margin-inline-end",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        MARGIN,
    ),
    p(
        "margin-inline-start",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        MARGIN,
    ),
    p(
        "margin-left",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        MARGIN,
    ),
    p(
        "margin-right",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        MARGIN,
    ),
    p(
        "margin-top",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        MARGIN,
    ),
    p("marker-end", Y, "none", A::SvgShapes, Cv::Generic, MARKER),
    p("marker-mid", Y, "none", A::SvgShapes, Cv::Generic, MARKER),
    p("marker-start", Y, "none", A::SvgShapes, Cv::Generic, MARKER),
    p(
        "mask-clip",
        N,
        "border-box",
        A::All,
        Cv::Generic,
        "[ content-box | padding-box | border-box | fill-box | stroke-box | view-box | no-clip ]#",
    ),
    p(
        "mask-composite",
        N,
        "add",
        A::All,
        Cv::Generic,
        "[ add | subtract | intersect | exclude ]#",
    ),
    p(
        "mask-image",
        N,
        "none",
        A::All,
        Cv::Image,
        "[ none | <image> ]#",
    ),
    p(
        "mask-mode",
        N,
        "match-source",
        A::All,
        Cv::Generic,
        "[ alpha | luminance | match-source ]#",
    ),
    p(
        "mask-origin",
        N,
        "border-box",
        A::All,
        Cv::Generic,
        "[ content-box | padding-box | border-box | fill-box | stroke-box | view-box ]#",
    ),
    p(
        "mask-position",
        N,
        "0% 0%",
        A::All,
        Cv::LengthPercentage,
        "<position>#",
    ),
    p(
        "mask-repeat",
        N,
        "repeat",
        A::All,
        Cv::Generic,
        "<repeat-style>#",
    ),
    p(
        "mask-size",
        N,
        "auto",
        A::All,
        Cv::LengthPercentage,
        "<bg-size>#",
    ),
    p(
        "mask-type",
        N,
        "luminance",
        A::Svg,
        Cv::Generic,
        "luminance | alpha",
    ),
    p(
        "math-depth",
        Y,
        "0",
        A::All,
        Cv::Integer,
        "auto-add | add( <integer> ) | <integer>",
    ),
    p(
        "math-shift",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | compact",
    ),
    p(
        "math-style",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | compact",
    ),
    p(
        "max-block-size",
        N,
        "none",
        A::Boxes,
        Cv::LengthPercentage,
        MAX_SIZE,
    ),
    p(
        "max-height",
        N,
        "none",
        A::Boxes,
        Cv::LengthPercentage,
        MAX_SIZE,
    ),
    p(
        "max-inline-size",
        N,
        "none",
        A::Boxes,
        Cv::LengthPercentage,
        MAX_SIZE,
    ),
    p(
        "max-width",
        N,
        "none",
        A::Boxes,
        Cv::LengthPercentage,
        MAX_SIZE,
    ),
    p(
        "min-block-size",
        N,
        "auto",
        A::Boxes,
        Cv::LengthPercentage,
        SIZE,
    ),
    p(
        "min-height",
        N,
        "auto",
        A::Boxes,
        Cv::LengthPercentage,
        SIZE,
    ),
    p(
        "min-inline-size",
        N,
        "auto",
        A::Boxes,
        Cv::LengthPercentage,
        SIZE,
    ),
    p("min-width", N, "auto", A::Boxes, Cv::LengthPercentage, SIZE),
    p(
        "mix-blend-mode",
        N,
        "normal",
        A::All,
        Cv::Generic,
        "<blend-mode> | plus-darker | plus-lighter",
    ),
    p(
        "object-fit",
        N,
        "fill",
        A::Replaced,
        Cv::Generic,
        "fill | contain | cover | none | scale-down",
    ),
    p(
        "object-position",
        N,
        "50% 50%",
        A::Replaced,
        Cv::LengthPercentage,
        "<position>",
    ),
    p(
        "offset-anchor",
        N,
        "auto",
        A::Transformable,
        Cv::LengthPercentage,
        "auto | <position>",
    ),
    p(
        "offset-distance",
        N,
        "0",
        A::Transformable,
        Cv::LengthPercentage,
        "<length-percentage>",
    ),
    p("offset-path", N, "none", A::Transformable, Cv::Generic, ""),
    p(
        "offset-position",
        N,
        "normal",
        A::Transformable,
        Cv::LengthPercentage,
        "normal | auto | <position>",
    ),
    p(
        "offset-rotate",
        N,
        "auto",
        A::Transformable,
        Cv::Generic,
        "[ auto | reverse ] || <angle>",
    ),
    p("opacity", N, "1", A::All, Cv::AlphaValue, ALPHA),
    p("order", N, "0", A::FlexGridItems, Cv::Integer, "<integer>"),
    p(
        "orphans",
        Y,
        "2",
        A::BlockContainers,
        Cv::Integer,
        "<integer [1,∞]>",
    ),
    p(
        "outline-color",
        N,
        "auto",
        A::All,
        Cv::Color,
        "auto | <color>",
    ),
    p("outline-offset", N, "0", A::All, Cv::Length, "<length>"),
    p(
        "outline-style",
        N,
        "none",
        A::All,
        Cv::Generic,
        "auto | <line-style>",
    ),
    p(
        "outline-width",
        N,
        "medium",
        A::All,
        Cv::LineWidth,
        LINE_WIDTH,
    ),
    p(
        "overflow-anchor",
        N,
        "auto",
        A::All,
        Cv::Generic,
        "auto | none",
    ),
    p(
        "overflow-block",
        N,
        "visible",
        A::BlockContainers,
        Cv::Generic,
        OVERFLOW,
    ),
    p(
        "overflow-clip-margin",
        N,
        "0px",
        A::All,
        Cv::Length,
        "<box> || <length [0,∞]>",
    ),
    p(
        "overflow-inline",
        N,
        "visible",
        A::BlockContainers,
        Cv::Generic,
        OVERFLOW,
    ),
    p(
        "overflow-wrap",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | break-word | anywhere",
    ),
    p(
        "overflow-x",
        N,
        "visible",
        A::BlockContainers,
        Cv::Generic,
        OVERFLOW,
    ),
    p(
        "overflow-y",
        N,
        "visible",
        A::BlockContainers,
        Cv::Generic,
        OVERFLOW,
    ),
    p(
        "overscroll-behavior-block",
        N,
        "auto",
        A::ScrollContainers,
        Cv::Generic,
        OVERSCROLL,
    ),
    p(
        "overscroll-behavior-inline",
        N,
        "auto",
        A::ScrollContainers,
        Cv::Generic,
        OVERSCROLL,
    ),
    p(
        "overscroll-behavior-x",
        N,
        "auto",
        A::ScrollContainers,
        Cv::Generic,
        OVERSCROLL,
    ),
    p(
        "overscroll-behavior-y",
        N,
        "auto",
        A::ScrollContainers,
        Cv::Generic,
        OVERSCROLL,
    ),
    p(
        "padding-block-end",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        PADDING,
    ),
    p(
        "padding-block-start",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        PADDING,
    ),
    p(
        "padding-bottom",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        PADDING,
    ),
    p(
        "padding-inline-end",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        PADDING,
    ),
    p(
        "padding-inline-start",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        PADDING,
    ),
    p(
        "padding-left",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        PADDING,
    ),
    p(
        "padding-right",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        PADDING,
    ),
    p(
        "padding-top",
        N,
        "0",
        A::NonTableInternal,
        Cv::LengthPercentage,
        PADDING,
    ),
    p(
        "page",
        N,
        "auto",
        A::BlockContainers,
        Cv::Generic,
        "auto | <custom-ident>",
    ),
    p(
        "paint-order",
        Y,
        "normal",
        A::SvgText,
        Cv::Generic,
        "normal | [ fill || stroke || markers ]",
    ),
    p(
        "perspective",
        N,
        "none",
        A::Transformable,
        Cv::Length,
        "none | <length [0,∞]>",
    ),
    p(
        "perspective-origin",
        N,
        "50% 50%",
        A::Transformable,
        Cv::LengthPercentage,
        "<position>",
    ),
    p(
        "pointer-events",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | bounding-box | visiblePainted | visibleFill | visibleStroke | visible | painted \
       | fill | stroke | all | none",
    ),
    p(
        "position",
        N,
        "static",
        A::All,
        Cv::Generic,
        "static | relative | absolute | sticky | fixed",
    ),
    p(
        "position-anchor",
        N,
        "auto",
        A::Positioned,
        Cv::Generic,
        "auto | <dashed-ident>",
    ),
    p(
        "print-color-adjust",
        Y,
        "economy",
        A::All,
        Cv::Generic,
        "economy | exact",
    ),
    p(
        "quotes",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | none | [ <string> <string> ]+",
    ),
    p(
        "r",
        N,
        "0",
        A::Svg,
        Cv::LengthPercentage,
        "<length-percentage [0,∞]>",
    ),
    p(
        "resize",
        N,
        "none",
        A::ScrollContainers,
        Cv::Generic,
        "none | both | horizontal | vertical | block | inline",
    ),
    p(
        "right",
        N,
        "auto",
        A::Positioned,
        Cv::LengthPercentage,
        INSET,
    ),
    p(
        "rotate",
        N,
        "none",
        A::Transformable,
        Cv::Generic,
        "none | <angle> | [ x | y | z | <number>{3} ] && <angle>",
    ),
    p(
        "row-gap",
        N,
        "normal",
        A::AlignContainers,
        Cv::LengthPercentage,
        "normal | <length-percentage [0,∞]>",
    ),
    p(
        "ruby-align",
        Y,
        "space-around",
        A::All,
        Cv::Generic,
        "start | center | space-between | space-around",
    ),
    p(
        "ruby-position",
        Y,
        "alternate",
        A::All,
        Cv::Generic,
        "[ alternate || [ over | under ] ] | inter-character",
    ),
    p(
        "rx",
        N,
        "auto",
        A::Svg,
        Cv::LengthPercentage,
        "auto | <length-percentage [0,∞]>",
    ),
    p(
        "ry",
        N,
        "auto",
        A::Svg,
        Cv::LengthPercentage,
        "auto | <length-percentage [0,∞]>",
    ),
    p(
        "scale",
        N,
        "none",
        A::Transformable,
        Cv::Generic,
        "none | [ <number> | <percentage> ]{1,3}",
    ),
    p(
        "scroll-behavior",
        N,
        "auto",
        A::ScrollContainers,
        Cv::Generic,
        "auto | smooth",
    ),
    p(
        "scroll-margin-block-end",
        N,
        "0",
        A::All,
        Cv::Length,
        "<length>",
    ),
    p(
        "scroll-margin-block-start",
        N,
        "0",
        A::All,
        Cv::Length,
        "<length>",
    ),
    p(
        "scroll-margin-bottom",
        N,
        "0",
        A::All,
        Cv::Length,
        "<length>",
    ),
    p(
        "scroll-margin-inline-end",
        N,
        "0",
        A::All,
        Cv::Length,
        "<length>",
    ),
    p(
        "scroll-margin-inline-start",
        N,
        "0",
        A::All,
        Cv::Length,
        "<length>",
    ),
    p("scroll-margin-left", N, "0", A::All, Cv::Length, "<length>"),
    p(
        "scroll-margin-right",
        N,
        "0",
        A::All,
        Cv::Length,
        "<length>",
    ),
    p("scroll-margin-top", N, "0", A::All, Cv::Length, "<length>"),
    p(
        "scroll-padding-block-end",
        N,
        "auto",
        A::ScrollContainers,
        Cv::LengthPercentage,
        SCROLL_PADDING,
    ),
    p(
        "scroll-padding-block-start",
        N,
        "auto",
        A::ScrollContainers,
        Cv::LengthPercentage,
        SCROLL_PADDING,
    ),
    p(
        "scroll-padding-bottom",
        N,
        "auto",
        A::ScrollContainers,
        Cv::LengthPercentage,
        SCROLL_PADDING,
    ),
    p(
        "scroll-padding-inline-end",
        N,
        "auto",
        A::ScrollContainers,
        Cv::LengthPercentage,
        SCROLL_PADDING,
    ),
    p(
        "scroll-padding-inline-start",
        N,
        "auto",
        A::ScrollContainers,
        Cv::LengthPercentage,
        SCROLL_PADDING,
    ),
    p(
        "scroll-padding-left",
        N,
        "auto",
        A::ScrollContainers,
        Cv::LengthPercentage,
        SCROLL_PADDING,
    ),
    p(
        "scroll-padding-right",
        N,
        "auto",
        A::ScrollContainers,
        Cv::LengthPercentage,
        SCROLL_PADDING,
    ),
    p(
        "scroll-padding-top",
        N,
        "auto",
        A::ScrollContainers,
        Cv::LengthPercentage,
        SCROLL_PADDING,
    ),
    p(
        "scroll-snap-align",
        N,
        "none",
        A::All,
        Cv::Generic,
        "[ none | start | end | center ]{1,2}",
    ),
    p(
        "scroll-snap-stop",
        N,
        "normal",
        A::All,
        Cv::Generic,
        "normal | always",
    ),
    p(
        "scroll-snap-type",
        N,
        "none",
        A::All,
        Cv::Generic,
        "none | [ x | y | block | inline | both ] [ mandatory | proximity ]?",
    ),
    p(
        "scrollbar-color",
        Y,
        "auto",
        A::ScrollContainers,
        Cv::Color,
        "auto | <color>{2}",
    ),
    p(
        "scrollbar-gutter",
        N,
        "auto",
        A::ScrollContainers,
        Cv::Generic,
        "auto | stable && both-edges?",
    ),
    p(
        "scrollbar-width",
        N,
        "auto",
        A::ScrollContainers,
        Cv::Generic,
        "auto | thin | none",
    ),
    p(
        "shape-image-threshold",
        N,
        "0",
        A::All,
        Cv::AlphaValue,
        ALPHA,
    ),
    p(
        "shape-margin",
        N,
        "0",
        A::All,
        Cv::LengthPercentage,
        "<length-percentage [0,∞]>",
    ),
    p("shape-outside", N, "none", A::All, Cv::Generic, ""),
    p(
        "shape-rendering",
        Y,
        "auto",
        A::SvgShapes,
        Cv::Generic,
        "auto | optimizeSpeed | crispEdges | geometricPrecision",
    ),
    p("stop-color", N, "black", A::Svg, Cv::Color, COLOR),
    p("stop-opacity", N, "1", A::Svg, Cv::AlphaValue, ALPHA),
    p("stroke", Y, "none", A::Svg, Cv::Color, "<paint>"),
    p(
        "stroke-dasharray",
        Y,
        "none",
        A::Svg,
        Cv::LengthPercentage,
        "none | [ [ <number [0,∞]> | <length-percentage [0,∞]> ]+ ]#",
    ),
    p(
        "stroke-dashoffset",
        Y,
        "0",
        A::Svg,
        Cv::LengthPercentage,
        "<number> | <length-percentage>",
    ),
    p(
        "stroke-linecap",
        Y,
        "butt",
        A::Svg,
        Cv::Generic,
        "butt | round | square",
    ),
    p(
        "stroke-linejoin",
        Y,
        "miter",
        A::Svg,
        Cv::Generic,
        "miter | miter-clip | round | bevel | arcs",
    ),
    p(
        "stroke-miterlimit",
        Y,
        "4",
        A::Svg,
        Cv::Number,
        "<number [0,∞]>",
    ),
    p("stroke-opacity", Y, "1", A::Svg, Cv::AlphaValue, ALPHA),
    p(
        "stroke-width",
        Y,
        "1px",
        A::Svg,
        Cv::LengthPercentage,
        "<number [0,∞]> | <length-percentage [0,∞]>",
    ),
    p(
        "tab-size",
        Y,
        "8",
        A::BlockContainers,
        Cv::Length,
        "<number [0,∞]> | <length [0,∞]>",
    ),
    p(
        "table-layout",
        N,
        "auto",
        A::Tables,
        Cv::Generic,
        "auto | fixed",
    ),
    p(
        "text-align",
        Y,
        "start",
        A::BlockContainers,
        Cv::Generic,
        "start | end | left | right | center | justify | match-parent | justify-all",
    ),
    p(
        "text-align-last",
        Y,
        "auto",
        A::BlockContainers,
        Cv::Generic,
        "auto | start | end | left | right | center | justify | match-parent",
    ),
    p(
        "text-anchor",
        Y,
        "start",
        A::SvgText,
        Cv::Generic,
        "start | middle | end",
    ),
    p(
        "text-combine-upright",
        N,
        "none",
        A::All,
        Cv::Generic,
        "none | all | digits <integer [2,4]>?",
    ),
    p(
        "text-decoration-color",
        N,
        "currentcolor",
        A::All,
        Cv::Color,
        COLOR,
    ),
    p(
        "text-decoration-line",
        N,
        "none",
        A::All,
        Cv::Generic,
        "none | [ underline || overline || line-through || blink ] | spelling-error | grammar-error",
    ),
    p(
        "text-decoration-skip-ink",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | none | all",
    ),
    p(
        "text-decoration-style",
        N,
        "solid",
        A::All,
        Cv::Generic,
        "solid | double | dotted | dashed | wavy",
    ),
    p(
        "text-decoration-thickness",
        N,
        "auto",
        A::All,
        Cv::LengthPercentage,
        "auto | from-font | <length-percentage>",
    ),
    p(
        "text-emphasis-color",
        Y,
        "currentcolor",
        A::All,
        Cv::Color,
        COLOR,
    ),
    p(
        "text-emphasis-position",
        Y,
        "over right",
        A::All,
        Cv::Generic,
        "[ over | under ] && [ right | left ]?",
    ),
    p(
        "text-emphasis-style",
        Y,
        "none",
        A::All,
        Cv::Generic,
        "none | [ [ filled | open ] || [ dot | circle | double-circle | triangle | sesame ] ] | <string>",
    ),
    p(
        "text-indent",
        Y,
        "0",
        A::BlockContainers,
        Cv::LengthPercentage,
        "<length-percentage> && hanging? && each-line?",
    ),
    p(
        "text-justify",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | none | inter-word | inter-character | distribute",
    ),
    p(
        "text-orientation",
        Y,
        "mixed",
        A::All,
        Cv::Generic,
        "mixed | upright | sideways | sideways-right",
    ),
    p(
        "text-overflow",
        N,
        "clip",
        A::BlockContainers,
        Cv::Generic,
        "[ clip | ellipsis | <string> ]{1,2}",
    ),
    p(
        "text-rendering",
        Y,
        "auto",
        A::SvgText,
        Cv::Generic,
        "auto | optimizeSpeed | optimizeLegibility | geometricPrecision",
    ),
    p(
        "text-shadow",
        Y,
        "none",
        A::All,
        Cv::Generic,
        "none | <text-shadow-item>#",
    ),
    p(
        "text-size-adjust",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | none | <percentage [0,∞]>",
    ),
    p(
        "text-transform",
        Y,
        "none",
        A::All,
        Cv::Generic,
        "none | [ capitalize | uppercase | lowercase ] || full-width || full-size-kana | math-auto",
    ),
    p(
        "text-underline-offset",
        Y,
        "auto",
        A::All,
        Cv::LengthPercentage,
        "auto | <length-percentage>",
    ),
    p(
        "text-underline-position",
        Y,
        "auto",
        A::All,
        Cv::Generic,
        "auto | [ from-font | under ] || [ left | right ]",
    ),
    p(
        "text-wrap-mode",
        Y,
        "wrap",
        A::All,
        Cv::Generic,
        "wrap | nowrap",
    ),
    p(
        "text-wrap-style",
        Y,
        "auto",
        A::BlockContainers,
        Cv::Generic,
        "auto | balance | stable | pretty | avoid-orphans",
    ),
    p("top", N, "auto", A::Positioned, Cv::LengthPercentage, INSET),
    p(
        "touch-action",
        N,
        "auto",
        A::All,
        Cv::Generic,
        "auto | none | [ [ pan-x | pan-left | pan-right ] || [ pan-y | pan-up | pan-down ] || pinch-zoom ] \
       | manipulation",
    ),
    p(
        "transform",
        N,
        "none",
        A::Transformable,
        Cv::Generic,
        "none | <transform-function>+",
    ),
    p(
        "transform-box",
        N,
        "view-box",
        A::Transformable,
        Cv::Generic,
        "content-box | border-box | fill-box | stroke-box | view-box",
    ),
    p(
        "transform-origin",
        N,
        "50% 50% 0px",
        A::Transformable,
        Cv::LengthPercentage,
        "[ left | center | right | top | bottom | <length-percentage> ] \
       | [ left | center | right | <length-percentage> ] [ top | center | bottom | <length-percentage> ] <length>? \
       | [ [ center | left | right ] && [ center | top | bottom ] ] <length>?",
    ),
    p(
        "transform-style",
        N,
        "flat",
        A::Transformable,
        Cv::Generic,
        "flat | preserve-3d",
    ),
    p(
        "transition-behavior",
        N,
        "normal",
        A::All,
        Cv::Generic,
        "[ normal | allow-discrete ]#",
    ),
    p("transition-delay", N, "0s", A::All, Cv::Generic, "<time>#"),
    p(
        "transition-duration",
        N,
        "0s",
        A::All,
        Cv::Generic,
        "<time [0,∞]>#",
    ),
    p(
        "transition-property",
        N,
        "all",
        A::All,
        Cv::Generic,
        "none | [ all | <custom-ident> ]#",
    ),
    p(
        "transition-timing-function",
        N,
        "ease",
        A::All,
        Cv::Generic,
        EASING,
    ),
    p(
        "translate",
        N,
        "none",
        A::Transformable,
        Cv::LengthPercentage,
        "none | <length-percentage> [ <length-percentage> <length>? ]?",
    ),
    p(
        "unicode-bidi",
        N,
        "normal",
        A::All,
        Cv::Generic,
        "normal | embed | isolate | bidi-override | isolate-override | plaintext",
    ),
    p(
        "user-select",
        N,
        "auto",
        A::All,
        Cv::Generic,
        "auto | text | none | contain | all",
    ),
    p(
        "vector-effect",
        N,
        "none",
        A::Svg,
        Cv::Generic,
        "none | non-scaling-stroke",
    ),
    p(
        "vertical-align",
        N,
        "baseline",
        A::InlineAndTableCells,
        Cv::LengthPercentage,
        "baseline | sub | super | text-top | text-bottom | middle | top | bottom | <length-percentage>",
    ),
    p(
        "view-transition-name",
        N,
        "none",
        A::All,
        Cv::Generic,
        "none | <custom-ident>",
    ),
    p(
        "visibility",
        Y,
        "visible",
        A::All,
        Cv::Generic,
        "visible | hidden | collapse",
    ),
    p(
        "white-space-collapse",
        Y,
        "collapse",
        A::All,
        Cv::Generic,
        "collapse | discard | preserve | preserve-breaks | preserve-spaces | break-spaces",
    ),
    p(
        "widows",
        Y,
        "2",
        A::BlockContainers,
        Cv::Integer,
        "<integer [1,∞]>",
    ),
    p("width", N, "auto", A::Boxes, Cv::LengthPercentage, SIZE),
    p(
        "will-change",
        N,
        "auto",
        A::All,
        Cv::Generic,
        "auto | [ scroll-position | contents | <custom-ident> ]#",
    ),
    p(
        "word-break",
        Y,
        "normal",
        A::All,
        Cv::Generic,
        "normal | keep-all | break-all | break-word | auto-phrase",
    ),
    p(
        "word-spacing",
        Y,
        "normal",
        A::All,
        Cv::LengthPercentage,
        "normal | <length-percentage>",
    ),
    p(
        "writing-mode",
        Y,
        "horizontal-tb",
        A::All,
        Cv::Generic,
        "horizontal-tb | vertical-rl | vertical-lr | sideways-rl | sideways-lr",
    ),
    p(
        "x",
        N,
        "0",
        A::Svg,
        Cv::LengthPercentage,
        "<length-percentage>",
    ),
    p(
        "y",
        N,
        "0",
        A::Svg,
        Cv::LengthPercentage,
        "<length-percentage>",
    ),
    p(
        "z-index",
        N,
        "auto",
        A::Positioned,
        Cv::Integer,
        "auto | <integer>",
    ),
    p(
        "zoom",
        N,
        "1",
        A::All,
        Cv::Generic,
        "normal | reset | <number [0,∞]> | <percentage [0,∞]>",
    ),
];

/// The definition of longhand `name`.
pub fn property(name: &str) -> Option<&'static Property> {
    PROPERTIES
        .binary_search_by(|p| p.name.cmp(name))
        .ok()
        .map(|i| &PROPERTIES[i])
}

/// The longhands of the shorthands kept as one declaration (their
/// expansion comes with the cascade).
pub const UNEXPANDED_SHORTHANDS: &[(&str, &[&str])] = &[
    (
        "animation",
        &[
            "animation-duration",
            "animation-timing-function",
            "animation-delay",
            "animation-iteration-count",
            "animation-direction",
            "animation-fill-mode",
            "animation-play-state",
            "animation-name",
            "animation-composition",
        ],
    ),
    (
        "background",
        &[
            "background-image",
            "background-position-x",
            "background-position-y",
            "background-size",
            "background-repeat",
            "background-attachment",
            "background-origin",
            "background-clip",
            "background-color",
        ],
    ),
    (
        "background-position",
        &["background-position-x", "background-position-y"],
    ),
    (
        "border-image",
        &[
            "border-image-source",
            "border-image-slice",
            "border-image-width",
            "border-image-outset",
            "border-image-repeat",
        ],
    ),
    ("container", &["container-name", "container-type"]),
    (
        "contain-intrinsic-size",
        &["contain-intrinsic-width", "contain-intrinsic-height"],
    ),
    (
        "font",
        &[
            "font-style",
            "font-variant-caps",
            "font-weight",
            "font-stretch",
            "font-size",
            "line-height",
            "font-family",
            "font-size-adjust",
            "font-kerning",
            "font-variant-alternates",
            "font-variant-east-asian",
            "font-variant-emoji",
            "font-variant-ligatures",
            "font-variant-numeric",
            "font-variant-position",
            "font-feature-settings",
            "font-language-override",
            "font-optical-sizing",
            "font-variation-settings",
            "font-palette",
        ],
    ),
    (
        "font-synthesis",
        &[
            "font-synthesis-weight",
            "font-synthesis-style",
            "font-synthesis-small-caps",
        ],
    ),
    (
        "font-variant",
        &[
            "font-variant-ligatures",
            "font-variant-caps",
            "font-variant-alternates",
            "font-variant-numeric",
            "font-variant-east-asian",
            "font-variant-position",
            "font-variant-emoji",
        ],
    ),
    (
        "grid",
        &[
            "grid-template-rows",
            "grid-template-columns",
            "grid-template-areas",
            "grid-auto-rows",
            "grid-auto-columns",
            "grid-auto-flow",
        ],
    ),
    (
        "grid-template",
        &[
            "grid-template-rows",
            "grid-template-columns",
            "grid-template-areas",
        ],
    ),
    (
        "mask",
        &[
            "mask-image",
            "mask-position",
            "mask-size",
            "mask-repeat",
            "mask-origin",
            "mask-clip",
            "mask-composite",
            "mask-mode",
        ],
    ),
    ("marker", &["marker-start", "marker-mid", "marker-end"]),
    (
        "offset",
        &[
            "offset-position",
            "offset-path",
            "offset-distance",
            "offset-rotate",
            "offset-anchor",
        ],
    ),
    (
        "text-emphasis",
        &["text-emphasis-style", "text-emphasis-color"],
    ),
    ("text-wrap", &["text-wrap-mode", "text-wrap-style"]),
    (
        "transition",
        &[
            "transition-property",
            "transition-duration",
            "transition-timing-function",
            "transition-delay",
            "transition-behavior",
        ],
    ),
    ("white-space", &["white-space-collapse", "text-wrap-mode"]),
];

/// The longhands of any shorthand: the expanded ones, the ones kept as
/// one declaration and `all` (every longhand but `direction` and
/// `unicode-bidi`).
pub fn longhands_of(shorthand_name: &str) -> Option<Vec<&'static str>> {
    if shorthand_name == "all" {
        return Some(
            PROPERTIES
                .iter()
                .map(|p| p.name)
                .filter(|n| *n != "direction" && *n != "unicode-bidi")
                .collect(),
        );
    }
    if let Some(s) = shorthand(shorthand_name) {
        return Some(s.longhands.iter().map(|(l, _)| *l).collect());
    }
    UNEXPANDED_SHORTHANDS
        .iter()
        .find(|(n, _)| *n == shorthand_name)
        .map(|(_, l)| l.to_vec())
}

// ----- logical properties ----------------------------------------------------------------

/// A side relative to the writing mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalSide {
    BlockStart,
    BlockEnd,
    InlineStart,
    InlineEnd,
}

/// Logical property groups: the logical names (block-start, block-end,
/// inline-start, inline-end) and the physical ones (top, right, bottom,
/// left).
pub static LOGICAL_SIDES: &[([&str; 4], [&str; 4])] = &[
    (
        [
            "margin-block-start",
            "margin-block-end",
            "margin-inline-start",
            "margin-inline-end",
        ],
        ["margin-top", "margin-right", "margin-bottom", "margin-left"],
    ),
    (
        [
            "padding-block-start",
            "padding-block-end",
            "padding-inline-start",
            "padding-inline-end",
        ],
        [
            "padding-top",
            "padding-right",
            "padding-bottom",
            "padding-left",
        ],
    ),
    (
        [
            "inset-block-start",
            "inset-block-end",
            "inset-inline-start",
            "inset-inline-end",
        ],
        ["top", "right", "bottom", "left"],
    ),
    (
        [
            "border-block-start-color",
            "border-block-end-color",
            "border-inline-start-color",
            "border-inline-end-color",
        ],
        [
            "border-top-color",
            "border-right-color",
            "border-bottom-color",
            "border-left-color",
        ],
    ),
    (
        [
            "border-block-start-style",
            "border-block-end-style",
            "border-inline-start-style",
            "border-inline-end-style",
        ],
        [
            "border-top-style",
            "border-right-style",
            "border-bottom-style",
            "border-left-style",
        ],
    ),
    (
        [
            "border-block-start-width",
            "border-block-end-width",
            "border-inline-start-width",
            "border-inline-end-width",
        ],
        [
            "border-top-width",
            "border-right-width",
            "border-bottom-width",
            "border-left-width",
        ],
    ),
    (
        [
            "scroll-margin-block-start",
            "scroll-margin-block-end",
            "scroll-margin-inline-start",
            "scroll-margin-inline-end",
        ],
        [
            "scroll-margin-top",
            "scroll-margin-right",
            "scroll-margin-bottom",
            "scroll-margin-left",
        ],
    ),
    (
        [
            "scroll-padding-block-start",
            "scroll-padding-block-end",
            "scroll-padding-inline-start",
            "scroll-padding-inline-end",
        ],
        [
            "scroll-padding-top",
            "scroll-padding-right",
            "scroll-padding-bottom",
            "scroll-padding-left",
        ],
    ),
];

/// Logical axis groups: (inline name, block name) and (horizontal name,
/// vertical name).
pub static LOGICAL_AXES: &[((&str, &str), (&str, &str))] = &[
    (("inline-size", "block-size"), ("width", "height")),
    (
        ("min-inline-size", "min-block-size"),
        ("min-width", "min-height"),
    ),
    (
        ("max-inline-size", "max-block-size"),
        ("max-width", "max-height"),
    ),
    (
        (
            "contain-intrinsic-inline-size",
            "contain-intrinsic-block-size",
        ),
        ("contain-intrinsic-width", "contain-intrinsic-height"),
    ),
    (
        ("overflow-inline", "overflow-block"),
        ("overflow-x", "overflow-y"),
    ),
    (
        ("overscroll-behavior-inline", "overscroll-behavior-block"),
        ("overscroll-behavior-x", "overscroll-behavior-y"),
    ),
];

/// The logical corners (start-start, start-end, end-start, end-end).
const LOGICAL_CORNERS: [&str; 4] = [
    "border-start-start-radius",
    "border-start-end-radius",
    "border-end-start-radius",
    "border-end-end-radius",
];

/// The physical side (0 top, 1 right, 2 bottom, 3 left) of a logical
/// side for a `writing-mode` and `direction`.
pub fn physical_side(side: LogicalSide, writing_mode: &str, rtl: bool) -> usize {
    let (block_start, inline_start) = match writing_mode {
        "vertical-rl" | "sideways-rl" => (1, if rtl { 2 } else { 0 }),
        "vertical-lr" => (3, if rtl { 2 } else { 0 }),
        "sideways-lr" => (3, if rtl { 0 } else { 2 }),
        _ => (0, if rtl { 1 } else { 3 }),
    };
    match side {
        LogicalSide::BlockStart => block_start,
        LogicalSide::BlockEnd => (block_start + 2) % 4,
        LogicalSide::InlineStart => inline_start,
        LogicalSide::InlineEnd => (inline_start + 2) % 4,
    }
}

/// The physical property a logical property stands for.
pub fn logical_to_physical(name: &str, writing_mode: &str, rtl: bool) -> Option<&'static str> {
    const SIDES: [LogicalSide; 4] = [
        LogicalSide::BlockStart,
        LogicalSide::BlockEnd,
        LogicalSide::InlineStart,
        LogicalSide::InlineEnd,
    ];
    for (logical, physical) in LOGICAL_SIDES {
        if let Some(i) = logical.iter().position(|l| *l == name) {
            return Some(physical[physical_side(SIDES[i], writing_mode, rtl)]);
        }
    }
    let horizontal = matches!(writing_mode, "horizontal-tb" | "");
    for ((inline, block), (h, v)) in LOGICAL_AXES {
        if name == *inline {
            return Some(if horizontal { *h } else { *v });
        }
        if name == *block {
            return Some(if horizontal { *v } else { *h });
        }
    }
    let corner = LOGICAL_CORNERS.iter().position(|c| *c == name)?;
    let block = if corner < 2 {
        LogicalSide::BlockStart
    } else {
        LogicalSide::BlockEnd
    };
    let inline = if corner % 2 == 0 {
        LogicalSide::InlineStart
    } else {
        LogicalSide::InlineEnd
    };
    let a = physical_side(block, writing_mode, rtl);
    let b = physical_side(inline, writing_mode, rtl);
    let (vertical, horizontal_side) = if a % 2 == 0 { (a, b) } else { (b, a) };
    Some(match (vertical, horizontal_side) {
        (0, 3) => "border-top-left-radius",
        (0, _) => "border-top-right-radius",
        (_, 3) => "border-bottom-left-radius",
        _ => "border-bottom-right-radius",
    })
}

/// True for a logical property of a logical group.
pub fn is_logical(name: &str) -> bool {
    logical_to_physical(name, "horizontal-tb", false).is_some()
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

    #[test]
    fn the_database_covers_every_longhand_and_its_grammars_parse() {
        use crate::css::values::grammar::{DEFINITIONS, parse_grammar};
        for name in LONGHANDS {
            assert!(
                property(name).is_some() || longhands_of(name).is_some(),
                "{name}"
            );
        }
        for w in PROPERTIES.windows(2) {
            assert!(w[0].name < w[1].name, "{} before {}", w[0].name, w[1].name);
        }
        for p in PROPERTIES {
            if let Some(g) = p.grammar {
                assert!(parse_grammar(g).is_some(), "{}", p.name);
            }
        }
        for (name, src) in DEFINITIONS {
            assert!(parse_grammar(src).is_some(), "<{name}>");
        }
        for (_, longhands) in UNEXPANDED_SHORTHANDS {
            for l in *longhands {
                assert!(property(l).is_some(), "{l}");
            }
        }
    }

    #[test]
    fn logical_properties_map_to_physical_ones() {
        assert_eq!(
            logical_to_physical("margin-inline-start", "horizontal-tb", true),
            Some("margin-right")
        );
        assert_eq!(
            logical_to_physical("inset-block-start", "vertical-rl", false),
            Some("right")
        );
        assert_eq!(
            logical_to_physical("inline-size", "vertical-lr", false),
            Some("height")
        );
        assert_eq!(
            logical_to_physical("border-start-end-radius", "horizontal-tb", false),
            Some("border-top-right-radius")
        );
        assert_eq!(
            logical_to_physical("border-end-start-radius", "vertical-rl", false),
            Some("border-top-left-radius")
        );
        assert!(!is_logical("margin-top"));
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
