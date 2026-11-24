//! Colors of CSS Color 4 and 5: named colors, hex colors, `rgb()`,
//! `hsl()`, `hwb()`, `lab()`, `lch()`, `oklab()`, `oklch()`, `color()`,
//! `color-mix()`, the relative color syntax (`from`), `light-dark()`,
//! `currentcolor`, `transparent` and the system colors. Conversions
//! between color spaces follow the sample code of CSS Color 4 §18 (D65
//! XYZ as the connection space, Bradford adaptation to D50), mixing
//! follows CSS Color 4 §12 (premultiplied alpha, missing components
//! carried forward) and serialization follows CSS Color 4 §15.

use super::calc::{self, Math, PercentAs, Resolver};
use super::number;
use crate::css::parser::ComponentValue;
use crate::css::tokenizer::Token;

// ----- color spaces ----------------------------------------------------------------------

/// The color spaces of CSS Color 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    Srgb,
    SrgbLinear,
    DisplayP3,
    DisplayP3Linear,
    A98Rgb,
    ProphotoRgb,
    Rec2020,
    XyzD50,
    XyzD65,
    Lab,
    Lch,
    Oklab,
    Oklch,
    Hsl,
    Hwb,
}

impl Space {
    /// The space named `name` in `color()` or a color interpolation
    /// method (ASCII case-insensitive).
    pub fn from_name(name: &str) -> Option<Space> {
        Some(match name.to_ascii_lowercase().as_str() {
            "srgb" => Space::Srgb,
            "srgb-linear" => Space::SrgbLinear,
            "display-p3" => Space::DisplayP3,
            "display-p3-linear" => Space::DisplayP3Linear,
            "a98-rgb" => Space::A98Rgb,
            "prophoto-rgb" => Space::ProphotoRgb,
            "rec2020" => Space::Rec2020,
            "xyz" | "xyz-d65" => Space::XyzD65,
            "xyz-d50" => Space::XyzD50,
            "lab" => Space::Lab,
            "lch" => Space::Lch,
            "oklab" => Space::Oklab,
            "oklch" => Space::Oklch,
            "hsl" => Space::Hsl,
            "hwb" => Space::Hwb,
            _ => return None,
        })
    }

    /// The serialized name of the space.
    pub fn name(self) -> &'static str {
        match self {
            Space::Srgb => "srgb",
            Space::SrgbLinear => "srgb-linear",
            Space::DisplayP3 => "display-p3",
            Space::DisplayP3Linear => "display-p3-linear",
            Space::A98Rgb => "a98-rgb",
            Space::ProphotoRgb => "prophoto-rgb",
            Space::Rec2020 => "rec2020",
            Space::XyzD50 => "xyz-d50",
            Space::XyzD65 => "xyz-d65",
            Space::Lab => "lab",
            Space::Lch => "lch",
            Space::Oklab => "oklab",
            Space::Oklch => "oklch",
            Space::Hsl => "hsl",
            Space::Hwb => "hwb",
        }
    }

    /// A space that uses polar coordinates (with a hue).
    pub fn is_polar(self) -> bool {
        matches!(self, Space::Lch | Space::Oklch | Space::Hsl | Space::Hwb)
    }

    /// A space of `color()`.
    pub fn is_predefined(self) -> bool {
        !matches!(
            self,
            Space::Lab | Space::Lch | Space::Oklab | Space::Oklch | Space::Hsl | Space::Hwb
        )
    }

    /// The index of the hue component.
    pub fn hue_index(self) -> Option<usize> {
        match self {
            Space::Hsl | Space::Hwb => Some(0),
            Space::Lch | Space::Oklch => Some(2),
            _ => None,
        }
    }

    /// The channel keywords of the relative color syntax.
    fn symbols(self) -> [&'static str; 3] {
        match self {
            Space::Hsl => ["h", "s", "l"],
            Space::Hwb => ["h", "w", "b"],
            Space::Lab | Space::Oklab => ["l", "a", "b"],
            Space::Lch | Space::Oklch => ["l", "c", "h"],
            Space::XyzD50 | Space::XyzD65 => ["x", "y", "z"],
            _ => ["r", "g", "b"],
        }
    }

    /// What 100% stands for in each component (CSS Color 4: lab()'s
    /// 100% lightness is 100 and 100% a is 125, …).
    fn percent_reference(self) -> [f64; 3] {
        match self {
            Space::Hsl | Space::Hwb => [0.0, 100.0, 100.0],
            Space::Lab => [100.0, 125.0, 125.0],
            Space::Lch => [100.0, 150.0, 0.0],
            Space::Oklab => [1.0, 0.4, 0.4],
            Space::Oklch => [1.0, 0.4, 0.0],
            _ => [1.0, 1.0, 1.0],
        }
    }

    fn analog(self, i: usize) -> Analog {
        use Analog::*;
        let row = match self {
            Space::Lab | Space::Oklab => [Lightness, OpponentA, OpponentB],
            Space::Lch | Space::Oklch => [Lightness, Colorfulness, Hue],
            Space::Hsl => [Hue, Colorfulness, Lightness],
            Space::Hwb => [Hue, Unrelated, Unrelated],
            _ => [Red, Green, Blue],
        };
        row[i]
    }
}

/// The analogous component categories of CSS Color 4 §12.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Analog {
    Red,
    Green,
    Blue,
    Lightness,
    Colorfulness,
    Hue,
    OpponentA,
    OpponentB,
    Unrelated,
}

// ----- conversions -----------------------------------------------------------------------

type M3 = [[f64; 3]; 3];

fn mul(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

const SRGB_TO_XYZ: M3 = [
    [506752.0 / 1228815.0, 87881.0 / 245763.0, 12673.0 / 70218.0],
    [87098.0 / 409605.0, 175762.0 / 245763.0, 12673.0 / 175545.0],
    [7918.0 / 409605.0, 87881.0 / 737289.0, 1001167.0 / 1053270.0],
];
const XYZ_TO_SRGB: M3 = [
    [12831.0 / 3959.0, -329.0 / 214.0, -1974.0 / 3959.0],
    [
        -851781.0 / 878810.0,
        1648619.0 / 878810.0,
        36519.0 / 878810.0,
    ],
    [705.0 / 12673.0, -2585.0 / 12673.0, 705.0 / 667.0],
];
const P3_TO_XYZ: M3 = [
    [
        608311.0 / 1250200.0,
        189793.0 / 714400.0,
        198249.0 / 1000160.0,
    ],
    [
        35783.0 / 156275.0,
        247089.0 / 357200.0,
        198249.0 / 2500400.0,
    ],
    [0.0, 32229.0 / 714400.0, 5220557.0 / 5000800.0],
];
const XYZ_TO_P3: M3 = [
    [
        446124.0 / 178915.0,
        -333277.0 / 357830.0,
        -72051.0 / 178915.0,
    ],
    [-14852.0 / 17905.0, 63121.0 / 35810.0, 423.0 / 17905.0],
    [11844.0 / 330415.0, -50337.0 / 660830.0, 316169.0 / 330415.0],
];
const A98_TO_XYZ: M3 = [
    [
        573536.0 / 994567.0,
        263643.0 / 1420810.0,
        187206.0 / 994567.0,
    ],
    [
        591459.0 / 1989134.0,
        6239551.0 / 9945670.0,
        374412.0 / 4972835.0,
    ],
    [
        53769.0 / 1989134.0,
        351524.0 / 4972835.0,
        4929758.0 / 4972835.0,
    ],
];
const XYZ_TO_A98: M3 = [
    [
        1829569.0 / 896150.0,
        -506331.0 / 2691145.0,
        -308931.0 / 896150.0,
    ],
    [
        -851781.0 / 878810.0,
        1648619.0 / 878810.0,
        36519.0 / 878810.0,
    ],
    [
        16779.0 / 1248040.0,
        -147721.0 / 1248040.0,
        1266979.0 / 1248040.0,
    ],
];
const PROPHOTO_TO_XYZ50: M3 = [
    [0.7977666449006423, 0.13518129740053308, 0.0313477341283922],
    [0.2880748288194013, 0.711835234241873, 0.00008993693872564],
    [0.0, 0.0, 0.8251046025104602],
];
const XYZ50_TO_PROPHOTO: M3 = [
    [
        1.3457868816471583,
        -0.25557208737979464,
        -0.05110186497554526,
    ],
    [-0.5446307051249019, 1.5082477428451468, 0.02052744743642139],
    [0.0, 0.0, 1.2119675456389452],
];
const REC2020_TO_XYZ: M3 = [
    [
        63426534.0 / 99577255.0,
        20160776.0 / 139408157.0,
        47086771.0 / 278816314.0,
    ],
    [
        26158966.0 / 99577255.0,
        472592308.0 / 697040785.0,
        8267143.0 / 139408157.0,
    ],
    [0.0, 19567812.0 / 697040785.0, 295819943.0 / 278816314.0],
];
const XYZ_TO_REC2020: M3 = [
    [
        30757411.0 / 17917100.0,
        -6372589.0 / 17917100.0,
        -4539589.0 / 17917100.0,
    ],
    [
        -19765991.0 / 29648200.0,
        47925759.0 / 29648200.0,
        467509.0 / 29648200.0,
    ],
    [
        792561.0 / 44930125.0,
        -1921689.0 / 44930125.0,
        42328811.0 / 44930125.0,
    ],
];
const D65_TO_D50: M3 = [
    [
        1.0479297925449969,
        0.022946870601609652,
        -0.05019226628920524,
    ],
    [
        0.02962780877005599,
        0.9904344267538799,
        -0.017073799063418826,
    ],
    [
        -0.009243040646204504,
        0.015055191490298152,
        0.7518742814281371,
    ],
];
const D50_TO_D65: M3 = [
    [0.955473421488075, -0.02309845494876471, 0.06325924320057072],
    [
        -0.0283697093338637,
        1.0099953980813041,
        0.021041441191917323,
    ],
    [
        0.012314014864481998,
        -0.020507649298898964,
        1.330365926242124,
    ],
];
const XYZ_TO_LMS: M3 = [
    [0.819022437996703, 0.3619062600528904, -0.1288737815209879],
    [0.0329836539323885, 0.9292868615863434, 0.0361446663506424],
    [0.0481771893596242, 0.2642395317527308, 0.6335478284694309],
];
const LMS_TO_OKLAB: M3 = [
    [0.210454268309314, 0.7936177747023054, -0.0040720430116193],
    [1.9779985324311684, -2.42859224204858, 0.450593709617411],
    [0.0259040424655478, 0.7827717124575296, -0.8086757660831043],
];
const OKLAB_TO_LMS: M3 = [
    [1.0, 0.3963377773761749, 0.2158037573099136],
    [1.0, -0.1055613458156586, -0.0638541728258133],
    [1.0, -0.0894841775298119, -1.2914855480194092],
];
const LMS_TO_XYZ: M3 = [
    [1.2268798758459243, -0.5578149944602171, 0.2813910456659647],
    [-0.0405757452148008, 1.112286803280317, -0.0717110580655164],
    [-0.0763729366746601, -0.4214933324022432, 1.5869240198367816],
];

const D50_WHITE: [f64; 3] = [0.3457 / 0.3585, 1.0, (1.0 - 0.3457 - 0.3585) / 0.3585];

fn srgb_lin(v: f64) -> f64 {
    let a = v.abs();
    if a <= 0.04045 {
        v / 12.92
    } else {
        v.signum() * ((a + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_gam(v: f64) -> f64 {
    let a = v.abs();
    if a > 0.0031308 {
        v.signum() * (1.055 * a.powf(1.0 / 2.4) - 0.055)
    } else {
        12.92 * v
    }
}

fn a98_lin(v: f64) -> f64 {
    v.signum() * v.abs().powf(563.0 / 256.0)
}

fn a98_gam(v: f64) -> f64 {
    v.signum() * v.abs().powf(256.0 / 563.0)
}

fn prophoto_lin(v: f64) -> f64 {
    let a = v.abs();
    if a <= 16.0 / 512.0 {
        v / 16.0
    } else {
        v.signum() * a.powf(1.8)
    }
}

fn prophoto_gam(v: f64) -> f64 {
    let a = v.abs();
    if a >= 1.0 / 512.0 {
        v.signum() * a.powf(1.0 / 1.8)
    } else {
        16.0 * v
    }
}

const REC_ALPHA: f64 = 1.09929682680944;
const REC_BETA: f64 = 0.018053968510807;

fn rec2020_lin(v: f64) -> f64 {
    let a = v.abs();
    if a < REC_BETA * 4.5 {
        v / 4.5
    } else {
        v.signum() * ((a + REC_ALPHA - 1.0) / REC_ALPHA).powf(1.0 / 0.45)
    }
}

fn rec2020_gam(v: f64) -> f64 {
    let a = v.abs();
    if a > REC_BETA {
        v.signum() * (REC_ALPHA * a.powf(0.45) - (REC_ALPHA - 1.0))
    } else {
        4.5 * v
    }
}

fn map3(v: [f64; 3], f: fn(f64) -> f64) -> [f64; 3] {
    [f(v[0]), f(v[1]), f(v[2])]
}

const LAB_EPSILON: f64 = 216.0 / 24389.0;
const LAB_KAPPA: f64 = 24389.0 / 27.0;

fn xyz50_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    let f = |i: usize| {
        let v = xyz[i] / D50_WHITE[i];
        if v > LAB_EPSILON {
            v.cbrt()
        } else {
            (LAB_KAPPA * v + 16.0) / 116.0
        }
    };
    let (f0, f1, f2) = (f(0), f(1), f(2));
    [116.0 * f1 - 16.0, 500.0 * (f0 - f1), 200.0 * (f1 - f2)]
}

fn lab_to_xyz50(lab: [f64; 3]) -> [f64; 3] {
    let f1 = (lab[0] + 16.0) / 116.0;
    let f0 = lab[1] / 500.0 + f1;
    let f2 = f1 - lab[2] / 200.0;
    let x = if f0.powi(3) > LAB_EPSILON {
        f0.powi(3)
    } else {
        (116.0 * f0 - 16.0) / LAB_KAPPA
    };
    let y = if lab[0] > LAB_KAPPA * LAB_EPSILON {
        ((lab[0] + 16.0) / 116.0).powi(3)
    } else {
        lab[0] / LAB_KAPPA
    };
    let z = if f2.powi(3) > LAB_EPSILON {
        f2.powi(3)
    } else {
        (116.0 * f2 - 16.0) / LAB_KAPPA
    };
    [x * D50_WHITE[0], y * D50_WHITE[1], z * D50_WHITE[2]]
}

fn xyz_to_oklab(xyz: [f64; 3]) -> [f64; 3] {
    let lms = mul(&XYZ_TO_LMS, xyz);
    mul(&LMS_TO_OKLAB, map3(lms, f64::cbrt))
}

fn oklab_to_xyz(lab: [f64; 3]) -> [f64; 3] {
    let lms = mul(&OKLAB_TO_LMS, lab);
    mul(&LMS_TO_XYZ, map3(lms, |v| v * v * v))
}

fn lab_to_lch(lab: [f64; 3], epsilon: f64) -> [f64; 3] {
    let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
    let h = if c <= epsilon {
        f64::NAN
    } else {
        lab[2].atan2(lab[1]).to_degrees().rem_euclid(360.0)
    };
    [lab[0], c, h]
}

fn lch_to_lab(lch: [f64; 3]) -> [f64; 3] {
    let h = if lch[2].is_nan() { 0.0 } else { lch[2] };
    let r = h.to_radians();
    [lch[0], lch[1] * r.cos(), lch[1] * r.sin()]
}

fn hsl_to_srgb(hsl: [f64; 3]) -> [f64; 3] {
    let h = if hsl[0].is_nan() {
        0.0
    } else {
        hsl[0].rem_euclid(360.0)
    };
    let s = hsl[1] / 100.0;
    let l = hsl[2] / 100.0;
    let f = |n: f64| {
        let k = (n + h / 30.0) % 12.0;
        let a = s * l.min(1.0 - l);
        l - a * (k - 3.0).min(9.0 - k).min(1.0).max(-1.0)
    };
    [f(0.0), f(8.0), f(4.0)]
}

fn srgb_to_hsl(rgb: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = rgb;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let light = (min + max) / 2.0;
    let d = max - min;
    let mut hue = f64::NAN;
    let mut sat = 0.0;
    if d != 0.0 {
        sat = if light == 0.0 || light == 1.0 {
            0.0
        } else {
            (max - light) / light.min(1.0 - light)
        };
        hue = if max == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        hue *= 60.0;
    }
    if sat < 0.0 {
        hue += 180.0;
        sat = sat.abs();
    }
    if hue >= 360.0 {
        hue -= 360.0;
    }
    if sat.abs() < 1e-9 {
        hue = f64::NAN;
    }
    [hue, sat * 100.0, light * 100.0]
}

fn hwb_to_srgb(hwb: [f64; 3]) -> [f64; 3] {
    let w = hwb[1] / 100.0;
    let b = hwb[2] / 100.0;
    if w + b >= 1.0 {
        let gray = w / (w + b);
        return [gray, gray, gray];
    }
    let rgb = hsl_to_srgb([hwb[0], 100.0, 50.0]);
    map3_with(rgb, |v| v * (1.0 - w - b) + w)
}

fn map3_with(v: [f64; 3], f: impl Fn(f64) -> f64) -> [f64; 3] {
    [f(v[0]), f(v[1]), f(v[2])]
}

fn srgb_to_hwb(rgb: [f64; 3]) -> [f64; 3] {
    let hsl = srgb_to_hsl(rgb);
    let white = rgb[0].min(rgb[1]).min(rgb[2]);
    let black = 1.0 - rgb[0].max(rgb[1]).max(rgb[2]);
    let hue = if white + black >= 1.0 - 1e-9 {
        f64::NAN
    } else {
        hsl[0]
    };
    [hue, white * 100.0, black * 100.0]
}

fn to_xyz(space: Space, c: [f64; 3]) -> [f64; 3] {
    match space {
        Space::Srgb => mul(&SRGB_TO_XYZ, map3(c, srgb_lin)),
        Space::SrgbLinear => mul(&SRGB_TO_XYZ, c),
        Space::DisplayP3 => mul(&P3_TO_XYZ, map3(c, srgb_lin)),
        Space::DisplayP3Linear => mul(&P3_TO_XYZ, c),
        Space::A98Rgb => mul(&A98_TO_XYZ, map3(c, a98_lin)),
        Space::ProphotoRgb => mul(&D50_TO_D65, mul(&PROPHOTO_TO_XYZ50, map3(c, prophoto_lin))),
        Space::Rec2020 => mul(&REC2020_TO_XYZ, map3(c, rec2020_lin)),
        Space::XyzD50 => mul(&D50_TO_D65, c),
        Space::XyzD65 => c,
        Space::Lab => mul(&D50_TO_D65, lab_to_xyz50(c)),
        Space::Lch => mul(&D50_TO_D65, lab_to_xyz50(lch_to_lab(c))),
        Space::Oklab => oklab_to_xyz(c),
        Space::Oklch => oklab_to_xyz(lch_to_lab(c)),
        Space::Hsl => to_xyz(Space::Srgb, hsl_to_srgb(c)),
        Space::Hwb => to_xyz(Space::Srgb, hwb_to_srgb(c)),
    }
}

fn from_xyz(space: Space, xyz: [f64; 3]) -> [f64; 3] {
    match space {
        Space::Srgb => map3(mul(&XYZ_TO_SRGB, xyz), srgb_gam),
        Space::SrgbLinear => mul(&XYZ_TO_SRGB, xyz),
        Space::DisplayP3 => map3(mul(&XYZ_TO_P3, xyz), srgb_gam),
        Space::DisplayP3Linear => mul(&XYZ_TO_P3, xyz),
        Space::A98Rgb => map3(mul(&XYZ_TO_A98, xyz), a98_gam),
        Space::ProphotoRgb => map3(mul(&XYZ50_TO_PROPHOTO, mul(&D65_TO_D50, xyz)), prophoto_gam),
        Space::Rec2020 => map3(mul(&XYZ_TO_REC2020, xyz), rec2020_gam),
        Space::XyzD50 => mul(&D65_TO_D50, xyz),
        Space::XyzD65 => xyz,
        Space::Lab => xyz50_to_lab(mul(&D65_TO_D50, xyz)),
        Space::Lch => lab_to_lch(xyz50_to_lab(mul(&D65_TO_D50, xyz)), 0.0015),
        Space::Oklab => xyz_to_oklab(xyz),
        Space::Oklch => lab_to_lch(xyz_to_oklab(xyz), 0.000004),
        Space::Hsl => srgb_to_hsl(from_xyz(Space::Srgb, xyz)),
        Space::Hwb => srgb_to_hwb(from_xyz(Space::Srgb, xyz)),
    }
}

/// Converts coordinates between spaces; a powerless hue comes out NaN.
pub fn convert_coords(from: Space, to: Space, c: [f64; 3]) -> [f64; 3] {
    if from == to {
        return c;
    }
    let srgb_family = |s: Space| matches!(s, Space::Srgb | Space::Hsl | Space::Hwb);
    if srgb_family(from) && srgb_family(to) {
        let rgb = match from {
            Space::Hsl => hsl_to_srgb(c),
            Space::Hwb => hwb_to_srgb(c),
            _ => c,
        };
        return match to {
            Space::Hsl => srgb_to_hsl(rgb),
            Space::Hwb => srgb_to_hwb(rgb),
            _ => rgb,
        };
    }
    match (from, to) {
        (Space::Lab, Space::Lch) => return lab_to_lch(c, 0.0015),
        (Space::Lch, Space::Lab) => return lch_to_lab(c),
        (Space::Oklab, Space::Oklch) => return lab_to_lch(c, 0.000004),
        (Space::Oklch, Space::Oklab) => return lch_to_lab(c),
        _ => {}
    }
    from_xyz(to, to_xyz(from, c))
}

// ----- absolute colors -------------------------------------------------------------------

/// A color with known components in a space; `None` is a missing
/// component (`none`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Absolute {
    pub space: Space,
    /// The components: sRGB-like spaces in [0, 1], `hsl()` and `hwb()` as
    /// hue in degrees and two percentages in [0, 100], Lab-like spaces in
    /// their own units.
    pub c: [Option<f64>; 3],
    pub alpha: Option<f64>,
    /// Written with a syntax that serializes as `rgb()`/`rgba()` (hex,
    /// named colors, `rgb()`, `hsl()`, `hwb()`, system colors).
    pub legacy: bool,
}

impl Absolute {
    /// An sRGB color from 8-bit channels, serialized as `rgb()`.
    pub fn rgb8(r: u8, g: u8, b: u8, alpha: f64) -> Absolute {
        Absolute {
            space: Space::Srgb,
            c: [
                Some(r as f64 / 255.0),
                Some(g as f64 / 255.0),
                Some(b as f64 / 255.0),
            ],
            alpha: Some(alpha),
            legacy: true,
        }
    }

    /// `transparent`.
    pub const TRANSPARENT: Absolute = Absolute {
        space: Space::Srgb,
        c: [Some(0.0), Some(0.0), Some(0.0)],
        alpha: Some(0.0),
        legacy: true,
    };

    /// The color in `to`, with missing components treated as zero and
    /// powerless hues missing.
    pub fn convert(&self, to: Space) -> Absolute {
        if self.space == to {
            return Absolute {
                legacy: false,
                ..*self
            };
        }
        let src = self.c.map(|v| v.unwrap_or(0.0));
        let out = convert_coords(self.space, to, src);
        Absolute {
            space: to,
            c: out.map(|v| if v.is_nan() { None } else { Some(v) }),
            alpha: self.alpha,
            legacy: false,
        }
    }

    /// sRGB components and alpha in [0, 1] (not clamped), missing ones as
    /// zero.
    pub fn to_srgb(&self) -> [f64; 4] {
        let c = self.convert(Space::Srgb);
        [
            c.c[0].unwrap_or(0.0),
            c.c[1].unwrap_or(0.0),
            c.c[2].unwrap_or(0.0),
            self.alpha.unwrap_or(0.0),
        ]
    }

    /// 8-bit sRGB channels and alpha, clamped (for painting).
    pub fn rgba8(&self) -> [u8; 4] {
        let s = self.to_srgb();
        s.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 1e-7).round() as u8)
    }

    /// Serializes the color (CSS Color 4 §15).
    pub fn to_css(&self) -> String {
        if self.legacy {
            let [r, g, b, _] = self.rgba8();
            let alpha = self.alpha.unwrap_or(0.0).clamp(0.0, 1.0);
            return if alpha == 1.0 {
                format!("rgb({r}, {g}, {b})")
            } else {
                format!("rgba({r}, {g}, {b}, {})", legacy_alpha(alpha))
            };
        }
        let comp = |v: Option<f64>| match v {
            Some(v) => number(v),
            None => "none".to_string(),
        };
        let alpha = match self.alpha {
            Some(a) if a == 1.0 => String::new(),
            Some(a) => format!(" / {}", number(a)),
            None => " / none".to_string(),
        };
        match self.space {
            Space::Lab | Space::Lch | Space::Oklab | Space::Oklch => format!(
                "{}({} {} {}{})",
                self.space.name(),
                comp(self.c[0]),
                comp(self.c[1]),
                comp(self.c[2]),
                alpha
            ),
            Space::Hsl | Space::Hwb => {
                let s = self.convert(Space::Srgb);
                format!(
                    "color(srgb {} {} {}{})",
                    comp(s.c[0]),
                    comp(s.c[1]),
                    comp(s.c[2]),
                    alpha
                )
            }
            space => format!(
                "color({} {} {} {}{})",
                space.name(),
                comp(self.c[0]),
                comp(self.c[1]),
                comp(self.c[2]),
                alpha
            ),
        }
    }
}

/// The alpha of a legacy color: two decimals when they keep the 8-bit
/// value, otherwise three.
fn legacy_alpha(a: f64) -> String {
    let two = (a * 100.0).round() / 100.0;
    if (two * 255.0).round() == (a * 255.0).round() {
        number(two)
    } else {
        number((a * 1000.0).round() / 1000.0)
    }
}

/// Clamps and normalizes the components of a color written in `space`.
fn build(space: Space, mut c: [Option<f64>; 3], alpha: Option<f64>, legacy: bool) -> Absolute {
    let clamp = |v: &mut Option<f64>, lo: f64, hi: f64| {
        if let Some(x) = v {
            *x = x.clamp(lo, hi);
        }
    };
    match space {
        Space::Lab => clamp(&mut c[0], 0.0, 100.0),
        Space::Lch => {
            clamp(&mut c[0], 0.0, 100.0);
            clamp(&mut c[1], 0.0, f64::INFINITY);
        }
        Space::Oklab => clamp(&mut c[0], 0.0, 1.0),
        Space::Oklch => {
            clamp(&mut c[0], 0.0, 1.0);
            clamp(&mut c[1], 0.0, f64::INFINITY);
        }
        Space::Hsl => clamp(&mut c[1], 0.0, f64::INFINITY),
        _ => {}
    }
    if let Some(h) = space.hue_index() {
        if let Some(x) = c[h] {
            c[h] = Some(if x.is_finite() {
                x.rem_euclid(360.0)
            } else {
                0.0
            });
        }
    }
    let alpha = alpha.map(|a| if a.is_nan() { 0.0 } else { a.clamp(0.0, 1.0) });
    Absolute {
        space,
        c,
        alpha,
        legacy,
    }
}

// ----- channels --------------------------------------------------------------------------

/// A component as written, before it is resolved.
#[derive(Clone, Debug, PartialEq)]
pub enum Channel {
    None,
    Number(f64),
    Percent(f64),
    /// An angle in degrees.
    Angle(f64),
    /// A channel keyword of the relative color syntax.
    Symbol(String),
    Calc(Box<Math>),
}

impl Channel {
    fn to_css(&self) -> String {
        match self {
            Channel::None => "none".to_string(),
            Channel::Number(v) => number(*v),
            Channel::Percent(v) => format!("{}%", number(*v)),
            Channel::Angle(v) => format!("{}deg", number(*v)),
            Channel::Symbol(s) => s.clone(),
            Channel::Calc(m) => m.to_css(),
        }
    }

    fn is_percent(&self) -> bool {
        match self {
            Channel::Percent(_) => true,
            Channel::Calc(m) => m.ty == calc::Type::PERCENT,
            _ => false,
        }
    }

    fn is_number(&self) -> bool {
        match self {
            Channel::Number(_) => true,
            Channel::Calc(m) => m.ty == calc::Type::NUMBER,
            _ => false,
        }
    }

    /// The value with 100% as `percent` and `symbol` resolving channel
    /// keywords; `Some(None)` for `none`.
    fn resolve(&self, percent: f64, symbol: &dyn Fn(&str) -> Option<f64>) -> Option<Option<f64>> {
        Some(match self {
            Channel::None => None,
            Channel::Number(v) | Channel::Angle(v) => Some(*v),
            Channel::Percent(p) => Some(p / 100.0 * percent),
            Channel::Symbol(s) => Some(symbol(s)?),
            Channel::Calc(m) => {
                let r = Resolver {
                    symbol: Some(symbol),
                    ..Resolver::default()
                };
                let (v, unit) = m.value(&r)?;
                match unit.as_str() {
                    "" | "deg" => Some(v),
                    "%" => Some(v / 100.0 * percent),
                    _ => return None,
                }
            }
        })
    }
}

/// Parses one component: a number, percentage, angle (`hue`), `none`, a
/// channel keyword or a math function of an allowed type.
fn channel(cv: &ComponentValue, hue: bool, symbols: &[&str]) -> Option<Channel> {
    match cv {
        ComponentValue::Token(Token::Number(n)) => Some(Channel::Number(n.value)),
        ComponentValue::Token(Token::Percentage(n)) if !hue => Some(Channel::Percent(n.value)),
        ComponentValue::Token(Token::Dimension(n, u)) if hue => {
            let (v, unit) = super::to_canonical(n.value, u)?;
            (unit == "deg").then_some(Channel::Angle(v))
        }
        ComponentValue::Token(Token::Ident(s)) => {
            let lower = s.to_ascii_lowercase();
            if lower == "none" {
                Some(Channel::None)
            } else if symbols.contains(&lower.as_str()) {
                Some(Channel::Symbol(lower))
            } else {
                None
            }
        }
        ComponentValue::Function(name, _) if calc::is_math_function(name) => {
            let m = calc::parse_with(cv, PercentAs::Percent, symbols)?;
            let ok = if hue {
                m.ty == calc::Type::NUMBER || m.ty == calc::Type::base(super::Category::Angle)
            } else {
                m.ty == calc::Type::NUMBER || m.ty == calc::Type::PERCENT
            };
            if !ok {
                return None;
            }
            // A function without channel keywords resolves now.
            if let Some((v, unit)) = m.value(&Resolver::default()) {
                return Some(match unit.as_str() {
                    "%" => Channel::Percent(v),
                    "deg" => Channel::Angle(v),
                    _ => Channel::Number(v),
                });
            }
            Some(Channel::Calc(Box::new(m)))
        }
        _ => None,
    }
}

fn no_symbol(_: &str) -> Option<f64> {
    None
}

// ----- the color value -------------------------------------------------------------------

/// The hue interpolation methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HueMethod {
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

impl HueMethod {
    fn name(self) -> &'static str {
        match self {
            HueMethod::Shorter => "shorter",
            HueMethod::Longer => "longer",
            HueMethod::Increasing => "increasing",
            HueMethod::Decreasing => "decreasing",
        }
    }
}

/// A `<color-interpolation-method>`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Interpolation {
    pub space: Space,
    pub hue: HueMethod,
    /// Written as `xyz` rather than `xyz-d65`.
    pub xyz_alias: bool,
}

impl Interpolation {
    /// Serializes the method (`in oklch longer hue`).
    pub fn to_css(&self) -> String {
        let name = if self.xyz_alias {
            "xyz"
        } else {
            self.space.name()
        };
        if self.space.is_polar() && self.hue != HueMethod::Shorter {
            format!("in {} {} hue", name, self.hue.name())
        } else {
            format!("in {name}")
        }
    }
}

/// Parses `in <space> [<hue-method> hue]?` from significant component
/// values.
pub fn parse_interpolation(v: &[&ComponentValue]) -> Option<Interpolation> {
    let ident = |i: usize| {
        v.get(i)
            .and_then(|c| c.ident())
            .map(str::to_ascii_lowercase)
    };
    if ident(0).as_deref() != Some("in") {
        return None;
    }
    let name = ident(1)?;
    let space = Space::from_name(&name)?;
    let hue = match v.len() {
        2 => HueMethod::Shorter,
        4 if space.is_polar() && ident(3).as_deref() == Some("hue") => match ident(2).as_deref() {
            Some("shorter") => HueMethod::Shorter,
            Some("longer") => HueMethod::Longer,
            Some("increasing") => HueMethod::Increasing,
            Some("decreasing") => HueMethod::Decreasing,
            _ => return None,
        },
        _ => return None,
    };
    Some(Interpolation {
        space,
        hue,
        xyz_alias: name == "xyz",
    })
}

/// A `color-mix()` function.
#[derive(Clone, Debug, PartialEq)]
pub struct Mix {
    /// `None` when the method was omitted (Oklab).
    pub method: Option<Interpolation>,
    pub colors: [(Color, Option<f64>); 2],
}

/// A color in the relative color syntax.
#[derive(Clone, Debug, PartialEq)]
pub struct Relative {
    pub space: Space,
    /// Written as `color(from … <space> …)`.
    pub color_function: bool,
    pub xyz_alias: bool,
    pub origin: Color,
    pub channels: [Channel; 3],
    pub alpha: Option<Channel>,
}

/// A `<color>` as specified.
#[derive(Clone, Debug, PartialEq)]
pub enum Color {
    Absolute(Absolute),
    /// A named color or `transparent` (lowercase).
    Named(String),
    CurrentColor,
    /// A system color (lowercase).
    System(String),
    Mix(Box<Mix>),
    Relative(Box<Relative>),
    LightDark(Box<Color>, Box<Color>),
}

/// What computing a color needs.
#[derive(Clone, Copy, Debug, Default)]
pub struct ColorContext {
    /// The value of `currentcolor`, when it is resolved.
    pub current: Option<Absolute>,
    /// The used color scheme is dark (`light-dark()`).
    pub dark: bool,
}

impl Color {
    /// Serializes the color as a specified value.
    pub fn to_css(&self) -> String {
        match self {
            Color::Absolute(a) => a.to_css(),
            Color::Named(n) | Color::System(n) => n.clone(),
            Color::CurrentColor => "currentcolor".to_string(),
            Color::LightDark(a, b) => format!("light-dark({}, {})", a.to_css(), b.to_css()),
            Color::Mix(m) => {
                let mut s = String::from("color-mix(");
                if let Some(method) = &m.method {
                    s.push_str(&method.to_css());
                    s.push_str(", ");
                }
                let [(c1, p1), (c2, p2)] = &m.colors;
                let (show1, show2) = match (p1, p2) {
                    (None, None) => (None, None),
                    (Some(a), None) => ((*a != 50.0).then_some(*a), None),
                    (None, Some(b)) => ((*b != 50.0).then_some(100.0 - b), None),
                    (Some(a), Some(b)) => {
                        if *a == 50.0 && *b == 50.0 {
                            (None, None)
                        } else if a + b == 100.0 {
                            (Some(*a), None)
                        } else {
                            (Some(*a), Some(*b))
                        }
                    }
                };
                s.push_str(&c1.to_css());
                if let Some(p) = show1 {
                    s.push_str(&format!(" {}%", number(p)));
                }
                s.push_str(", ");
                s.push_str(&c2.to_css());
                if let Some(p) = show2 {
                    s.push_str(&format!(" {}%", number(p)));
                }
                s.push(')');
                s
            }
            Color::Relative(r) => {
                let channels = r
                    .channels
                    .iter()
                    .map(Channel::to_css)
                    .collect::<Vec<_>>()
                    .join(" ");
                let alpha = match &r.alpha {
                    Some(a) => format!(" / {}", a.to_css()),
                    None => String::new(),
                };
                if r.color_function {
                    let name = if r.xyz_alias { "xyz" } else { r.space.name() };
                    format!(
                        "color(from {} {} {}{})",
                        r.origin.to_css(),
                        name,
                        channels,
                        alpha
                    )
                } else {
                    let name = match r.space {
                        Space::Srgb => "rgb",
                        other => other.name(),
                    };
                    format!("{}(from {} {}{})", name, r.origin.to_css(), channels, alpha)
                }
            }
        }
    }

    /// The computed color: named and system colors become absolute,
    /// `light-dark()` picks a side, and mixes and relative colors resolve
    /// when their colors are absolute. `currentcolor` stays unless `ctx`
    /// resolves it.
    pub fn compute(&self, ctx: &ColorContext) -> Color {
        match self {
            Color::Absolute(_) => self.clone(),
            Color::Named(n) => match named(n) {
                Some(a) => Color::Absolute(a),
                None => self.clone(),
            },
            Color::System(n) => match system(n) {
                Some(a) => Color::Absolute(a),
                None => self.clone(),
            },
            Color::CurrentColor => match ctx.current {
                Some(a) => Color::Absolute(a),
                None => Color::CurrentColor,
            },
            Color::LightDark(a, b) => {
                if ctx.dark {
                    b.compute(ctx)
                } else {
                    a.compute(ctx)
                }
            }
            Color::Mix(m) => {
                let c1 = m.colors[0].0.compute(ctx);
                let c2 = m.colors[1].0.compute(ctx);
                if let (Color::Absolute(a), Color::Absolute(b)) = (&c1, &c2) {
                    if let Some(r) = mix(m.method.as_ref(), a, m.colors[0].1, b, m.colors[1].1) {
                        return Color::Absolute(r);
                    }
                }
                Color::Mix(Box::new(Mix {
                    method: m.method,
                    colors: [(c1, m.colors[0].1), (c2, m.colors[1].1)],
                }))
            }
            Color::Relative(r) => {
                let origin = r.origin.compute(ctx);
                if let Color::Absolute(o) = &origin {
                    if let Some(c) = resolve_relative(r, o) {
                        return Color::Absolute(c);
                    }
                }
                Color::Relative(Box::new(Relative {
                    origin,
                    ..(**r).clone()
                }))
            }
        }
    }

    /// The absolute color, with `current` as `currentcolor`.
    pub fn resolve(&self, current: Absolute, dark: bool) -> Option<Absolute> {
        let ctx = ColorContext {
            current: Some(current),
            dark,
        };
        match self.compute(&ctx) {
            Color::Absolute(a) => Some(a),
            _ => None,
        }
    }

    /// True if the color depends on `currentcolor`.
    pub fn uses_currentcolor(&self) -> bool {
        match self {
            Color::CurrentColor => true,
            Color::Mix(m) => m.colors.iter().any(|(c, _)| c.uses_currentcolor()),
            Color::Relative(r) => r.origin.uses_currentcolor(),
            Color::LightDark(a, b) => a.uses_currentcolor() || b.uses_currentcolor(),
            _ => false,
        }
    }
}

// ----- mixing ----------------------------------------------------------------------------

/// The interpolation fraction of the second color and the alpha
/// multiplier of `color-mix()` percentages (CSS Color 5 §2.1).
pub fn mix_weights(p1: Option<f64>, p2: Option<f64>) -> Option<(f64, f64)> {
    let (p1, p2) = match (p1, p2) {
        (None, None) => (50.0, 50.0),
        (Some(a), None) => (a, 100.0 - a),
        (None, Some(b)) => (100.0 - b, b),
        (Some(a), Some(b)) => (a, b),
    };
    let sum = p1 + p2;
    if sum <= 0.0 {
        return None;
    }
    let multiplier = if sum < 100.0 { sum / 100.0 } else { 1.0 };
    Some((p2 / sum, multiplier))
}

/// `c` converted to `space` with its missing components carried forward
/// to their analogous components.
fn carry_forward(c: &Absolute, space: Space) -> Absolute {
    let mut out = c.convert(space);
    for j in 0..3 {
        if c.c[j].is_none() {
            let category = c.space.analog(j);
            if category == Analog::Unrelated {
                continue;
            }
            for i in 0..3 {
                if space.analog(i) == category {
                    out.c[i] = None;
                }
            }
        }
    }
    out
}

/// Interpolates `a` and `b` in `space` at `t` (CSS Color 4 §12).
pub fn interpolate(space: Space, hue: HueMethod, a: &Absolute, b: &Absolute, t: f64) -> Absolute {
    let mut x = carry_forward(a, space);
    let mut y = carry_forward(b, space);
    for i in 0..3 {
        match (x.c[i], y.c[i]) {
            (None, Some(v)) => x.c[i] = Some(v),
            (Some(v), None) => y.c[i] = Some(v),
            _ => {}
        }
    }
    match (x.alpha, y.alpha) {
        (None, Some(v)) => x.alpha = Some(v),
        (Some(v), None) => y.alpha = Some(v),
        _ => {}
    }
    let hue_index = space.hue_index();
    let ax = x.alpha.unwrap_or(1.0);
    let ay = y.alpha.unwrap_or(1.0);
    let mut out = [None; 3];
    for i in 0..3 {
        let (Some(mut p), Some(mut q)) = (x.c[i], y.c[i]) else {
            continue;
        };
        if Some(i) == hue_index {
            p = p.rem_euclid(360.0);
            q = q.rem_euclid(360.0);
            match hue {
                HueMethod::Shorter => {
                    if q - p > 180.0 {
                        p += 360.0;
                    } else if q - p < -180.0 {
                        q += 360.0;
                    }
                }
                HueMethod::Longer => {
                    if 0.0 < q - p && q - p < 180.0 {
                        p += 360.0;
                    } else if -180.0 < q - p && q - p <= 0.0 {
                        q += 360.0;
                    }
                }
                HueMethod::Increasing => {
                    if q < p {
                        q += 360.0;
                    }
                }
                HueMethod::Decreasing => {
                    if p < q {
                        p += 360.0;
                    }
                }
            }
            out[i] = Some((p + (q - p) * t).rem_euclid(360.0));
        } else {
            out[i] = Some(p * ax * (1.0 - t) + q * ay * t);
        }
    }
    let alpha = ax * (1.0 - t) + ay * t;
    if alpha != 0.0 {
        for (i, v) in out.iter_mut().enumerate() {
            if Some(i) != hue_index {
                if let Some(v) = v {
                    *v /= alpha;
                }
            }
        }
    }
    Absolute {
        space,
        c: out,
        alpha: if x.alpha.is_none() && y.alpha.is_none() {
            None
        } else {
            Some(alpha)
        },
        legacy: false,
    }
}

/// The color of `color-mix()` with absolute colors: in the interpolation
/// space, serialized as `rgb()` for `hsl` and `hwb`.
pub fn mix(
    method: Option<&Interpolation>,
    a: &Absolute,
    p1: Option<f64>,
    b: &Absolute,
    p2: Option<f64>,
) -> Option<Absolute> {
    let (t, multiplier) = mix_weights(p1, p2)?;
    let (space, hue) = match method {
        Some(m) => (m.space, m.hue),
        None => (Space::Oklab, HueMethod::Shorter),
    };
    let mut r = interpolate(space, hue, a, b, t);
    if let Some(alpha) = r.alpha.as_mut() {
        *alpha *= multiplier;
    }
    r.legacy = matches!(space, Space::Hsl | Space::Hwb);
    Some(r)
}

/// The color of a relative color with an absolute origin.
fn resolve_relative(r: &Relative, origin: &Absolute) -> Option<Absolute> {
    let o = origin.convert(r.space);
    let scale = if r.space == Space::Srgb && !r.color_function {
        255.0
    } else {
        1.0
    };
    let names = r.space.symbols();
    let values = [
        o.c[0].unwrap_or(0.0) * scale,
        o.c[1].unwrap_or(0.0) * scale,
        o.c[2].unwrap_or(0.0) * scale,
    ];
    let alpha = origin.alpha.unwrap_or(0.0);
    let symbol = move |s: &str| -> Option<f64> {
        if s == "alpha" {
            return Some(alpha);
        }
        names.iter().position(|n| *n == s).map(|i| values[i])
    };
    let refs = if scale == 255.0 {
        [255.0; 3]
    } else {
        r.space.percent_reference()
    };
    let mut c = [None; 3];
    for i in 0..3 {
        c[i] = r.channels[i].resolve(refs[i], &symbol)?.map(|v| v / scale);
    }
    let a = match &r.alpha {
        Some(ch) => ch.resolve(1.0, &symbol)?,
        None => origin.alpha,
    };
    Some(build(r.space, c, a, false))
}

// ----- parsing ---------------------------------------------------------------------------

/// Parses a `<color>`.
pub fn parse(cv: &ComponentValue) -> Option<Color> {
    match cv {
        ComponentValue::Token(Token::Hash { value, .. }) => hex(value).map(Color::Absolute),
        ComponentValue::Token(Token::Ident(s)) => {
            let lower = s.to_ascii_lowercase();
            if lower == "currentcolor" {
                return Some(Color::CurrentColor);
            }
            if named(&lower).is_some() {
                return Some(Color::Named(lower));
            }
            if system(&lower).is_some() {
                return Some(Color::System(lower));
            }
            None
        }
        ComponentValue::Function(name, args) => {
            let lower = name.to_ascii_lowercase();
            let sig: Vec<&ComponentValue> = args.iter().filter(|c| !c.is_whitespace()).collect();
            match lower.as_str() {
                "rgb" | "rgba" => color_function(Space::Srgb, &sig, true),
                "hsl" | "hsla" => color_function(Space::Hsl, &sig, true),
                "hwb" => color_function(Space::Hwb, &sig, false),
                "lab" => color_function(Space::Lab, &sig, false),
                "lch" => color_function(Space::Lch, &sig, false),
                "oklab" => color_function(Space::Oklab, &sig, false),
                "oklch" => color_function(Space::Oklch, &sig, false),
                "color" => predefined(&sig),
                "color-mix" => parse_mix(&sig),
                "light-dark" => {
                    let parts = split_commas(&sig);
                    match parts.as_slice() {
                        [[a], [b]] => {
                            Some(Color::LightDark(Box::new(parse(a)?), Box::new(parse(b)?)))
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Parses a color from a CSS string (`"rgb(1 2 3)"`).
pub fn parse_str(text: &str) -> Option<Color> {
    let values = crate::css::parser::parse_component_values(text);
    let sig: Vec<&ComponentValue> = values.iter().filter(|c| !c.is_whitespace()).collect();
    match sig.as_slice() {
        [cv] => parse(cv),
        _ => None,
    }
}

fn split_commas<'a, 'b>(v: &'b [&'a ComponentValue]) -> Vec<&'b [&'a ComponentValue]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in v.iter().enumerate() {
        if matches!(c, ComponentValue::Token(Token::Comma)) {
            out.push(&v[start..i]);
            start = i + 1;
        }
    }
    out.push(&v[start..]);
    out
}

fn hex(value: &str) -> Option<Absolute> {
    if !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let digit = |i: usize| u8::from_str_radix(&value[i..i + 1], 16).ok();
    let pair = |i: usize| u8::from_str_radix(&value[i..i + 2], 16).ok();
    let (r, g, b, a) = match value.len() {
        3 | 4 => {
            let r = digit(0)? * 17;
            let g = digit(1)? * 17;
            let b = digit(2)? * 17;
            let a = if value.len() == 4 {
                digit(3)? * 17
            } else {
                255
            };
            (r, g, b, a)
        }
        6 | 8 => {
            let a = if value.len() == 8 { pair(6)? } else { 255 };
            (pair(0)?, pair(2)?, pair(4)?, a)
        }
        _ => return None,
    };
    Some(Absolute::rgb8(r, g, b, a as f64 / 255.0))
}

/// The modern syntax: three components and an optional `/ alpha`.
fn modern(
    v: &[&ComponentValue],
    space: Space,
    symbols: &[&str],
) -> Option<([Channel; 3], Option<Channel>)> {
    let slash = v
        .iter()
        .position(|c| matches!(c, ComponentValue::Token(Token::Delim('/'))));
    let (comps, alpha) = match slash {
        Some(i) => (&v[..i], Some(&v[i + 1..])),
        None => (v, None),
    };
    if comps.len() != 3 {
        return None;
    }
    let hue = space.hue_index();
    let c0 = channel(comps[0], hue == Some(0), symbols)?;
    let c1 = channel(comps[1], hue == Some(1), symbols)?;
    let c2 = channel(comps[2], hue == Some(2), symbols)?;
    let alpha = match alpha {
        Some([a]) => Some(channel(a, false, symbols)?),
        Some(_) => return None,
        None => None,
    };
    Some(([c0, c1, c2], alpha))
}

fn resolve_absolute(
    space: Space,
    channels: &[Channel; 3],
    alpha: Option<&Channel>,
    scale: f64,
) -> Option<([Option<f64>; 3], Option<f64>)> {
    let refs = if scale == 255.0 {
        [255.0; 3]
    } else {
        space.percent_reference()
    };
    let mut c = [None; 3];
    for i in 0..3 {
        c[i] = channels[i].resolve(refs[i], &no_symbol)?.map(|v| v / scale);
    }
    let a = match alpha {
        Some(ch) => ch.resolve(1.0, &no_symbol)?,
        None => Some(1.0),
    };
    Some((c, a))
}

/// `rgb()`, `hsl()`, `hwb()`, `lab()`, `lch()`, `oklab()` and `oklch()`,
/// with the legacy comma syntax when `legacy_syntax`.
fn color_function(space: Space, v: &[&ComponentValue], legacy_syntax: bool) -> Option<Color> {
    let scale = if space == Space::Srgb { 255.0 } else { 1.0 };
    if v.first().is_some_and(|c| super::is_ident(c, "from")) {
        let origin = parse(v.get(1)?)?;
        let mut symbols: Vec<&str> = space.symbols().to_vec();
        symbols.push("alpha");
        let (channels, alpha) = modern(&v[2..], space, &symbols)?;
        return Some(Color::Relative(Box::new(Relative {
            space,
            color_function: false,
            xyz_alias: false,
            origin,
            channels,
            alpha,
        })));
    }
    let legacy = matches!(space, Space::Srgb | Space::Hsl | Space::Hwb);
    if legacy_syntax
        && v.iter()
            .any(|c| matches!(c, ComponentValue::Token(Token::Comma)))
    {
        let parts = split_commas(v);
        if parts.len() != 3 && parts.len() != 4 {
            return None;
        }
        let mut chans = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            let [cv] = p else { return None };
            let hue = space == Space::Hsl && i == 0;
            let ch = channel(cv, hue, &[])?;
            if matches!(ch, Channel::None) {
                return None;
            }
            chans.push(ch);
        }
        let alpha = if chans.len() == 4 { chans.pop() } else { None };
        if let Some(a) = &alpha {
            if !a.is_number() && !a.is_percent() {
                return None;
            }
        }
        let ok = if space == Space::Srgb {
            chans.iter().all(Channel::is_number) || chans.iter().all(Channel::is_percent)
        } else {
            chans[1].is_percent() && chans[2].is_percent()
        };
        if !ok {
            return None;
        }
        let channels = [chans[0].clone(), chans[1].clone(), chans[2].clone()];
        let (c, a) = resolve_absolute(space, &channels, alpha.as_ref(), scale)?;
        return Some(Color::Absolute(build(space, c, a, true)));
    }
    let (channels, alpha) = modern(v, space, &[])?;
    let (c, a) = resolve_absolute(space, &channels, alpha.as_ref(), scale)?;
    Some(Color::Absolute(build(space, c, a, legacy)))
}

/// `color()`.
fn predefined(v: &[&ComponentValue]) -> Option<Color> {
    let (origin, rest) = if v.first().is_some_and(|c| super::is_ident(c, "from")) {
        (Some(parse(v.get(1)?)?), &v[2..])
    } else {
        (None, v)
    };
    let name = rest.first()?.ident()?.to_ascii_lowercase();
    let space = Space::from_name(&name).filter(|s| s.is_predefined())?;
    let xyz_alias = name == "xyz";
    match origin {
        Some(origin) => {
            let mut symbols: Vec<&str> = space.symbols().to_vec();
            symbols.push("alpha");
            let (channels, alpha) = modern(&rest[1..], space, &symbols)?;
            Some(Color::Relative(Box::new(Relative {
                space,
                color_function: true,
                xyz_alias,
                origin,
                channels,
                alpha,
            })))
        }
        None => {
            let (channels, alpha) = modern(&rest[1..], space, &[])?;
            let (c, a) = resolve_absolute(space, &channels, alpha.as_ref(), 1.0)?;
            Some(Color::Absolute(build(space, c, a, false)))
        }
    }
}

/// A `<percentage [0,100]>` of `color-mix()`.
fn mix_percentage(cv: &ComponentValue) -> Option<f64> {
    match cv {
        ComponentValue::Token(Token::Percentage(n)) => {
            (0.0..=100.0).contains(&n.value).then_some(n.value)
        }
        ComponentValue::Function(name, _) if calc::is_math_function(name) => {
            let m = calc::parse(cv, PercentAs::Percent)?;
            if m.ty != calc::Type::PERCENT {
                return None;
            }
            let (v, _) = m.value(&Resolver::default())?;
            Some(v.clamp(0.0, 100.0))
        }
        _ => None,
    }
}

/// `color-mix()`.
fn parse_mix(v: &[&ComponentValue]) -> Option<Color> {
    let parts = split_commas(v);
    let (method, colors) = match parts.first() {
        Some(first) if first.first().is_some_and(|c| super::is_ident(c, "in")) => {
            (Some(parse_interpolation(first)?), &parts[1..])
        }
        _ => (None, &parts[..]),
    };
    if colors.len() != 2 {
        return None;
    }
    let mut out = Vec::with_capacity(2);
    for part in colors {
        let (color, pct) = match part {
            [c] => (parse(c)?, None),
            [a, b] => match parse(a) {
                Some(c) => (c, Some(mix_percentage(b)?)),
                None => (parse(b)?, Some(mix_percentage(a)?)),
            },
            _ => return None,
        };
        out.push((color, pct));
    }
    if let (Some(a), Some(b)) = (out[0].1, out[1].1) {
        if a + b == 0.0 {
            return None;
        }
    }
    let second = out.pop()?;
    let first = out.pop()?;
    Some(Color::Mix(Box::new(Mix {
        method,
        colors: [first, second],
    })))
}

// ----- named and system colors -----------------------------------------------------------

/// The named colors of CSS Color 4 §6.1.
pub static NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

fn from_hex(v: u32) -> Absolute {
    Absolute::rgb8((v >> 16) as u8, (v >> 8) as u8, v as u8, 1.0)
}

/// The color of a named color or `transparent` (lowercase name).
pub fn named(name: &str) -> Option<Absolute> {
    if name == "transparent" {
        return Some(Absolute::TRANSPARENT);
    }
    NAMED
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| from_hex(*v))
}

/// The system colors of CSS Color 4 §6.2 with the light values vidrio
/// uses, and the deprecated system colors of §6.2.1 mapped to them.
pub static SYSTEM: &[(&str, u32)] = &[
    ("accentcolor", 0x0060df),
    ("accentcolortext", 0xffffff),
    ("activetext", 0xee0000),
    ("buttonborder", 0x767676),
    ("buttonface", 0xe9e9ed),
    ("buttontext", 0x000000),
    ("canvas", 0xffffff),
    ("canvastext", 0x000000),
    ("field", 0xffffff),
    ("fieldtext", 0x000000),
    ("graytext", 0x6d6d6d),
    ("highlight", 0x3399ff),
    ("highlighttext", 0xffffff),
    ("linktext", 0x0000ee),
    ("mark", 0xffff00),
    ("marktext", 0x000000),
    ("selecteditem", 0x3399ff),
    ("selecteditemtext", 0xffffff),
    ("visitedtext", 0x551a8b),
    // Deprecated system colors.
    ("activeborder", 0x767676),
    ("activecaption", 0xffffff),
    ("appworkspace", 0xffffff),
    ("background", 0xffffff),
    ("buttonhighlight", 0x767676),
    ("buttonshadow", 0x767676),
    ("captiontext", 0x000000),
    ("inactiveborder", 0x767676),
    ("inactivecaption", 0xffffff),
    ("inactivecaptiontext", 0x6d6d6d),
    ("infobackground", 0xffffff),
    ("infotext", 0x000000),
    ("menu", 0xffffff),
    ("menutext", 0x000000),
    ("scrollbar", 0xffffff),
    ("threeddarkshadow", 0x767676),
    ("threedface", 0xe9e9ed),
    ("threedhighlight", 0x767676),
    ("threedlightshadow", 0x767676),
    ("threedshadow", 0x767676),
    ("window", 0xffffff),
    ("windowframe", 0x767676),
    ("windowtext", 0x000000),
];

/// The color of a system color (lowercase name).
pub fn system(name: &str) -> Option<Absolute> {
    SYSTEM
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| from_hex(*v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn computed(text: &str) -> String {
        parse_str(text)
            .expect(text)
            .compute(&ColorContext::default())
            .to_css()
    }

    #[test]
    fn legacy_colors_serialize_as_rgb() {
        assert_eq!(computed("#f00"), "rgb(255, 0, 0)");
        assert_eq!(computed("#ff000080"), "rgba(255, 0, 0, 0.5)");
        assert_eq!(computed("RED"), "rgb(255, 0, 0)");
        assert_eq!(computed("transparent"), "rgba(0, 0, 0, 0)");
        assert_eq!(computed("rgb(0 128 255 / 50%)"), "rgba(0, 128, 255, 0.5)");
        assert_eq!(
            computed("rgba(10%, 20%, 30%, 0.25)"),
            "rgba(26, 51, 77, 0.25)"
        );
        assert_eq!(computed("hsl(120deg 100% 50%)"), "rgb(0, 255, 0)");
        assert_eq!(computed("hwb(0 0% 0%)"), "rgb(255, 0, 0)");
        assert!(parse_str("rgb(1, 2 3)").is_none());
        assert!(parse_str("rgb(1, 2%, 3)").is_none());
        assert!(parse_str("hsl(1, 2, 3)").is_none());
    }

    #[test]
    fn specified_keywords_are_kept() {
        assert_eq!(parse_str("Red").unwrap().to_css(), "red");
        assert_eq!(parse_str("CurrentColor").unwrap().to_css(), "currentcolor");
    }

    #[test]
    fn modern_spaces_serialize_in_their_space() {
        assert_eq!(computed("lab(50% 25% -25%)"), "lab(50 31.25 -31.25)");
        assert_eq!(computed("lch(120 30 400)"), "lch(100 30 40)");
        assert_eq!(
            computed("oklch(0.5 0.1 none / 0.5)"),
            "oklch(0.5 0.1 none / 0.5)"
        );
        assert_eq!(
            computed("color(xyz 0.1 0.2 0.3)"),
            "color(xyz-d65 0.1 0.2 0.3)"
        );
    }
}
