//! Name adjustments for foreign content (HTML §13.2.6.1 and §13.2.6.5):
//! SVG tag and attribute case, MathML `definitionURL`, and namespaced
//! attributes.

use crate::dom::{Atom, Attribute, Namespace};

/// SVG element names that are not all lowercase (§13.2.6.5).
const SVG_TAGS: &[(&str, &str)] = &[
    ("altglyph", "altGlyph"),
    ("altglyphdef", "altGlyphDef"),
    ("altglyphitem", "altGlyphItem"),
    ("animatecolor", "animateColor"),
    ("animatemotion", "animateMotion"),
    ("animatetransform", "animateTransform"),
    ("clippath", "clipPath"),
    ("feblend", "feBlend"),
    ("fecolormatrix", "feColorMatrix"),
    ("fecomponenttransfer", "feComponentTransfer"),
    ("fecomposite", "feComposite"),
    ("feconvolvematrix", "feConvolveMatrix"),
    ("fediffuselighting", "feDiffuseLighting"),
    ("fedisplacementmap", "feDisplacementMap"),
    ("fedistantlight", "feDistantLight"),
    ("fedropshadow", "feDropShadow"),
    ("feflood", "feFlood"),
    ("fefunca", "feFuncA"),
    ("fefuncb", "feFuncB"),
    ("fefuncg", "feFuncG"),
    ("fefuncr", "feFuncR"),
    ("fegaussianblur", "feGaussianBlur"),
    ("feimage", "feImage"),
    ("femerge", "feMerge"),
    ("femergenode", "feMergeNode"),
    ("femorphology", "feMorphology"),
    ("feoffset", "feOffset"),
    ("fepointlight", "fePointLight"),
    ("fespecularlighting", "feSpecularLighting"),
    ("fespotlight", "feSpotLight"),
    ("fetile", "feTile"),
    ("feturbulence", "feTurbulence"),
    ("foreignobject", "foreignObject"),
    ("glyphref", "glyphRef"),
    ("lineargradient", "linearGradient"),
    ("radialgradient", "radialGradient"),
    ("textpath", "textPath"),
];

/// SVG attribute names that are not all lowercase (§13.2.6.1).
const SVG_ATTRIBUTES: &[(&str, &str)] = &[
    ("attributename", "attributeName"),
    ("attributetype", "attributeType"),
    ("basefrequency", "baseFrequency"),
    ("baseprofile", "baseProfile"),
    ("calcmode", "calcMode"),
    ("clippathunits", "clipPathUnits"),
    ("diffuseconstant", "diffuseConstant"),
    ("edgemode", "edgeMode"),
    ("filterunits", "filterUnits"),
    ("glyphref", "glyphRef"),
    ("gradienttransform", "gradientTransform"),
    ("gradientunits", "gradientUnits"),
    ("kernelmatrix", "kernelMatrix"),
    ("kernelunitlength", "kernelUnitLength"),
    ("keypoints", "keyPoints"),
    ("keysplines", "keySplines"),
    ("keytimes", "keyTimes"),
    ("lengthadjust", "lengthAdjust"),
    ("limitingconeangle", "limitingConeAngle"),
    ("markerheight", "markerHeight"),
    ("markerunits", "markerUnits"),
    ("markerwidth", "markerWidth"),
    ("maskcontentunits", "maskContentUnits"),
    ("maskunits", "maskUnits"),
    ("numoctaves", "numOctaves"),
    ("pathlength", "pathLength"),
    ("patterncontentunits", "patternContentUnits"),
    ("patterntransform", "patternTransform"),
    ("patternunits", "patternUnits"),
    ("pointsatx", "pointsAtX"),
    ("pointsaty", "pointsAtY"),
    ("pointsatz", "pointsAtZ"),
    ("preservealpha", "preserveAlpha"),
    ("preserveaspectratio", "preserveAspectRatio"),
    ("primitiveunits", "primitiveUnits"),
    ("refx", "refX"),
    ("refy", "refY"),
    ("repeatcount", "repeatCount"),
    ("repeatdur", "repeatDur"),
    ("requiredextensions", "requiredExtensions"),
    ("requiredfeatures", "requiredFeatures"),
    ("specularconstant", "specularConstant"),
    ("specularexponent", "specularExponent"),
    ("spreadmethod", "spreadMethod"),
    ("startoffset", "startOffset"),
    ("stddeviation", "stdDeviation"),
    ("stitchtiles", "stitchTiles"),
    ("surfacescale", "surfaceScale"),
    ("systemlanguage", "systemLanguage"),
    ("tablevalues", "tableValues"),
    ("targetx", "targetX"),
    ("targety", "targetY"),
    ("textlength", "textLength"),
    ("viewbox", "viewBox"),
    ("viewtarget", "viewTarget"),
    ("xchannelselector", "xChannelSelector"),
    ("ychannelselector", "yChannelSelector"),
    ("zoomandpan", "zoomAndPan"),
];

/// Attributes that become namespaced in foreign content (§13.2.6.1):
/// token name, prefix, local name, namespace.
const FOREIGN_ATTRIBUTES: &[(&str, Option<&str>, &str, Namespace)] = &[
    ("xlink:actuate", Some("xlink"), "actuate", Namespace::XLink),
    ("xlink:arcrole", Some("xlink"), "arcrole", Namespace::XLink),
    ("xlink:href", Some("xlink"), "href", Namespace::XLink),
    ("xlink:role", Some("xlink"), "role", Namespace::XLink),
    ("xlink:show", Some("xlink"), "show", Namespace::XLink),
    ("xlink:title", Some("xlink"), "title", Namespace::XLink),
    ("xlink:type", Some("xlink"), "type", Namespace::XLink),
    ("xml:lang", Some("xml"), "lang", Namespace::Xml),
    ("xml:space", Some("xml"), "space", Namespace::Xml),
    ("xmlns", None, "xmlns", Namespace::Xmlns),
    ("xmlns:xlink", Some("xmlns"), "xlink", Namespace::Xmlns),
];

/// The SVG element name for the token tag name `name`.
pub fn svg_tag_name(name: Atom) -> Atom {
    match SVG_TAGS.iter().find(|(from, _)| name == *from) {
        Some((_, to)) => Atom::new(to),
        None => name,
    }
}

/// "Adjust SVG attributes".
pub fn adjust_svg_attributes(attrs: &mut [Attribute]) {
    for attr in attrs {
        if attr.ns.is_none() {
            if let Some((_, to)) = SVG_ATTRIBUTES.iter().find(|(from, _)| attr.local == *from) {
                attr.local = Atom::new(to);
            }
        }
    }
}

/// "Adjust MathML attributes".
pub fn adjust_mathml_attributes(attrs: &mut [Attribute]) {
    for attr in attrs {
        if attr.ns.is_none() && attr.local == "definitionurl" {
            attr.local = Atom::new("definitionURL");
        }
    }
}

/// "Adjust foreign attributes".
pub fn adjust_foreign_attributes(attrs: &mut [Attribute]) {
    for attr in attrs {
        if attr.ns.is_some() {
            continue;
        }
        if let Some((_, prefix, local, ns)) = FOREIGN_ATTRIBUTES
            .iter()
            .find(|(name, ..)| attr.local == *name)
        {
            attr.prefix = prefix.map(Atom::new);
            attr.local = Atom::new(local);
            attr.ns = Some(*ns);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjustments() {
        assert_eq!(svg_tag_name(Atom::new("foreignobject")), "foreignObject");
        assert_eq!(svg_tag_name(Atom::new("rect")), "rect");
        let mut attrs = vec![
            Attribute::new("viewbox", "0 0 1 1"),
            Attribute::new("xlink:href", "#a"),
            Attribute::new("definitionurl", "u"),
            Attribute::new("xmlns", "x"),
        ];
        adjust_svg_attributes(&mut attrs);
        adjust_mathml_attributes(&mut attrs);
        adjust_foreign_attributes(&mut attrs);
        assert_eq!(attrs[0].local, "viewBox");
        assert_eq!(attrs[1].ns, Some(Namespace::XLink));
        assert_eq!(attrs[1].prefix.map(|p| p.as_str()), Some("xlink"));
        assert_eq!(attrs[1].local, "href");
        assert_eq!(attrs[2].local, "definitionURL");
        assert_eq!(attrs[3].ns, Some(Namespace::Xmlns));
        assert_eq!(attrs[3].prefix, None);
    }
}
