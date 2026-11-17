//! WHATWG Encoding, URL and MIME type parsing: invalid sequences in UTF-8,
//! Shift_JIS, GB18030 and ISO-2022-JP; odd IPv4 hosts, compressed IPv6,
//! IDNA and per-component percent-encoding; MIME types with quoted
//! parameters. Expected values come from the specifications, the pinned
//! indexes and the WPT data files (`url/resources/urltestdata.json`,
//! `mimesniff/mime-types/resources/mime-types.json`).

use vidrio::encoding::{
    self, BomHandling, Decoder, DecoderErrorMode, Encoder, EncoderErrorMode, Encoding,
};
use vidrio::mime::MimeType;
use vidrio::url::{Url, form};

fn decode(encoding: Encoding, bytes: &[u8]) -> String {
    encoding::decode_without_bom(bytes, encoding)
}

fn decode_fatal(encoding: Encoding, bytes: &[u8]) -> Option<String> {
    let mut decoder = Decoder::new(encoding, DecoderErrorMode::Fatal, BomHandling::Keep);
    let mut out = String::new();
    decoder.decode(bytes, true, &mut out).ok().map(|()| out)
}

/// Decode `bytes` one byte per call, to exercise the streaming state.
fn decode_bytewise(encoding: Encoding, bytes: &[u8]) -> String {
    let mut decoder = Decoder::new(encoding, DecoderErrorMode::Replacement, BomHandling::Keep);
    let mut out = String::new();
    for &b in bytes {
        decoder.decode(&[b], false, &mut out).unwrap();
    }
    decoder.decode(&[], true, &mut out).unwrap();
    out
}

fn encode_html(encoding: Encoding, text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    Encoder::new(encoding)
        .encode(text, true, &mut out, EncoderErrorMode::Html)
        .unwrap();
    out
}

// ----- Encoding -----

#[test]
fn labels_resolve_case_insensitively_after_trimming() {
    assert_eq!(Encoding::for_label(" \tUTF8\n"), Some(Encoding::Utf8));
    assert_eq!(Encoding::for_label("latin1"), Some(Encoding::Windows1252));
    assert_eq!(Encoding::for_label("Shift_JIS"), Some(Encoding::ShiftJis));
    assert_eq!(Encoding::for_label("x-sjis"), Some(Encoding::ShiftJis));
    assert_eq!(Encoding::for_label("gb2312"), Some(Encoding::Gbk));
    assert_eq!(
        Encoding::for_label("csiso2022jp"),
        Some(Encoding::Iso2022Jp)
    );
    assert_eq!(Encoding::for_label("utf-16"), Some(Encoding::Utf16Le));
    assert_eq!(
        Encoding::for_label("iso-2022-kr"),
        Some(Encoding::Replacement)
    );
    assert_eq!(Encoding::for_label("utf-7"), None);
    assert_eq!(Encoding::for_label(""), None);
    for &e in Encoding::all() {
        assert_eq!(Encoding::for_label(e.name()), Some(e), "{}", e.name());
    }
    assert_eq!(Encoding::all().len(), 40);
}

#[test]
fn bom_sniffing_overrides_the_fallback() {
    assert_eq!(
        encoding::decode(b"\xEF\xBB\xBFa", Encoding::Windows1252),
        ("a".to_string(), Encoding::Utf8)
    );
    assert_eq!(
        encoding::decode(b"\xFE\xFF\x00a", Encoding::Utf8),
        ("a".to_string(), Encoding::Utf16Be)
    );
    assert_eq!(
        encoding::decode(b"\xFF\xFEa\x00", Encoding::Utf8),
        ("a".to_string(), Encoding::Utf16Le)
    );
    assert_eq!(
        encoding::decode(b"\xE9", Encoding::Windows1252),
        ("\u{E9}".to_string(), Encoding::Windows1252)
    );
    assert_eq!(
        encoding::utf8_decode(b"\xEF\xBB\xBF\xEF\xBB\xBF"),
        "\u{FEFF}"
    );
}

#[test]
fn utf8_invalid_sequences() {
    let r = '\u{FFFD}';
    // Overlong, surrogate, above U+10FFFF: one U+FFFD per byte.
    assert_eq!(decode(Encoding::Utf8, b"\xC0\x80"), format!("{r}{r}"));
    assert_eq!(
        decode(Encoding::Utf8, b"\xE0\x80\x80"),
        format!("{r}{r}{r}")
    );
    assert_eq!(
        decode(Encoding::Utf8, b"\xED\xA0\x80"),
        format!("{r}{r}{r}")
    );
    assert_eq!(
        decode(Encoding::Utf8, b"\xF4\x90\x80\x80"),
        format!("{r}{r}{r}{r}")
    );
    // A truncated sequence is one error, and the byte that broke it is
    // read again.
    assert_eq!(decode(Encoding::Utf8, b"a\xF0\x9F\x98"), format!("a{r}"));
    assert_eq!(decode(Encoding::Utf8, b"\xE2\x82A"), format!("{r}A"));
    assert_eq!(decode(Encoding::Utf8, b"\xFF\xFE"), format!("{r}{r}"));
    assert_eq!(
        decode_bytewise(Encoding::Utf8, b"\xF0\x9F\x98\x80\xE2\x82A"),
        format!("\u{1F600}{r}A")
    );
    assert_eq!(decode_fatal(Encoding::Utf8, b"ok\xC3"), None);
    assert_eq!(
        decode_fatal(Encoding::Utf8, b"ok\xC3\xA9").as_deref(),
        Some("ok\u{E9}")
    );
    assert_eq!(encoding::utf8_decode_without_bom_or_fail(b"\xFF"), None);
}

#[test]
fn shift_jis_invalid_sequences() {
    let r = '\u{FFFD}';
    assert_eq!(
        decode(Encoding::ShiftJis, b"\x82\xA0\x93\xFA"),
        "\u{3042}\u{65E5}"
    );
    // An ASCII trail byte is not a valid trail and is read again.
    assert_eq!(decode(Encoding::ShiftJis, b"\x81\x20"), format!("{r} "));
    // 0xA0 and 0xFD to 0xFF are never valid; 0x80 is U+0080.
    assert_eq!(
        decode(Encoding::ShiftJis, b"\xA0\xFD\x80"),
        format!("{r}{r}\u{80}")
    );
    // Half-width katakana and the EUDC range.
    assert_eq!(
        decode(Encoding::ShiftJis, b"\xB1\xF0\x40"),
        "\u{FF71}\u{E000}"
    );
    // A lead byte at the end of the input.
    assert_eq!(decode(Encoding::ShiftJis, b"a\x82"), format!("a{r}"));
    assert_eq!(
        decode_bytewise(Encoding::ShiftJis, b"\x82\xA0\x82"),
        format!("\u{3042}{r}")
    );
    assert_eq!(decode_fatal(Encoding::ShiftJis, b"\x82\x20"), None);
    assert_eq!(
        encode_html(Encoding::ShiftJis, "\u{65E5}\u{A5}\u{2603}"),
        b"\x93\xFA\x5C&#9731;"
    );
}

#[test]
fn gb18030_invalid_sequences() {
    let r = '\u{FFFD}';
    assert_eq!(decode(Encoding::Gb18030, b"\x81\x30\x81\x30"), "\u{80}");
    assert_eq!(decode(Encoding::Gb18030, b"\xE3\x32\x9A\x35"), "\u{10FFFF}");
    assert_eq!(
        decode(Encoding::Gb18030, b"\x80\xA3\xA0"),
        "\u{20AC}\u{3000}"
    );
    // A four-byte pointer between the BMP and supplementary ranges.
    assert_eq!(
        decode(Encoding::Gb18030, b"\x84\x31\xA5\x30"),
        format!("{r}")
    );
    // A broken four-byte sequence restores its second, third and fourth bytes.
    assert_eq!(decode(Encoding::Gb18030, b"\x81\x30\x41"), format!("{r}0A"));
    assert_eq!(
        decode(Encoding::Gb18030, b"\x81\x30\x81\x41"),
        format!("{r}0\u{4E04}")
    );
    assert_eq!(decode(Encoding::Gb18030, b"\xFF"), format!("{r}"));
    assert_eq!(decode(Encoding::Gb18030, b"\x81"), format!("{r}"));
    assert_eq!(
        decode_bytewise(Encoding::Gb18030, b"\xE3\x32\x9A\x35\x81\x30"),
        format!("\u{10FFFF}{r}")
    );
    assert_eq!(decode_fatal(Encoding::Gb18030, b"\x81\x30\x81"), None);
    assert_eq!(
        encode_html(Encoding::Gb18030, "\u{10FFFF}"),
        b"\xE3\x32\x9A\x35"
    );
    assert_eq!(encode_html(Encoding::Gbk, "\u{10FFFF}"), b"&#1114111;");
}

#[test]
fn iso_2022_jp_invalid_sequences() {
    let r = '\u{FFFD}';
    assert_eq!(
        decode(Encoding::Iso2022Jp, b"\x1B$B\x24\x22\x1B(B"),
        "\u{3042}"
    );
    assert_eq!(
        decode(Encoding::Iso2022Jp, b"\x1B(J\x5C\x7E"),
        "\u{A5}\u{203E}"
    );
    assert_eq!(decode(Encoding::Iso2022Jp, b"\x1B(I\x21"), "\u{FF61}");
    // Two escape sequences in a row with nothing between them.
    assert_eq!(decode(Encoding::Iso2022Jp, b"\x1B(J\x1B(B"), format!("{r}"));
    // An unknown escape sequence: its bytes are read again as ASCII.
    assert_eq!(decode(Encoding::Iso2022Jp, b"\x1B(Z"), format!("{r}(Z"));
    // SO and SI are never valid.
    assert_eq!(decode(Encoding::Iso2022Jp, b"a\x0E"), format!("a{r}"));
    // A lone lead byte and a lone ESC at the end.
    assert_eq!(decode(Encoding::Iso2022Jp, b"\x1B$B\x24"), format!("{r}"));
    assert_eq!(decode(Encoding::Iso2022Jp, b"\x1B"), format!("{r}"));
    // ESC in the middle of a two-byte character.
    assert_eq!(
        decode(Encoding::Iso2022Jp, b"\x1B$B\x24\x1B(Bx"),
        format!("{r}x")
    );
    assert_eq!(
        decode_bytewise(Encoding::Iso2022Jp, b"\x1B$B\x24\x22\x1B(B!"),
        "\u{3042}!"
    );
    assert_eq!(decode_fatal(Encoding::Iso2022Jp, b"\x1B(Z"), None);
    assert_eq!(
        encode_html(Encoding::Iso2022Jp, "\u{65E5}a"),
        b"\x1B$BF|\x1B(Ba"
    );
    assert_eq!(
        encode_html(Encoding::Iso2022Jp, "\u{FF71}"),
        b"\x1B$B%\x22\x1B(B"
    );
}

#[test]
fn utf16_and_single_byte_and_others() {
    let r = '\u{FFFD}';
    // A lone surrogate and an odd byte count.
    assert_eq!(decode(Encoding::Utf16Le, b"\x00\xD8a\x00"), format!("{r}a"));
    assert_eq!(
        decode(Encoding::Utf16Be, b"\xD8\x3D\xDE\x00\x00"),
        format!("\u{1F600}{r}")
    );
    assert_eq!(decode(Encoding::Windows1252, b"\x80\x81"), "\u{20AC}\u{81}");
    assert_eq!(decode(Encoding::Iso8859_3, b"\xA5"), format!("{r}"));
    assert_eq!(decode(Encoding::Replacement, b"abc"), format!("{r}"));
    assert_eq!(decode(Encoding::Replacement, b""), "");
    assert_eq!(
        decode(Encoding::XUserDefined, b"a\x80\xFF"),
        "a\u{F780}\u{F7FF}"
    );
    assert_eq!(
        decode(Encoding::EucKr, b"\xB0\xA1\x80"),
        format!("\u{AC00}{r}")
    );
    assert_eq!(
        decode(Encoding::Big5, b"\xA4\x40\x88\x62"),
        "\u{4E00}\u{CA}\u{304}"
    );
    assert_eq!(
        decode(Encoding::EucJp, b"\x8E\xB1\xA4\xA2"),
        "\u{FF71}\u{3042}"
    );
    assert_eq!(
        encoding::encode("\u{E9}\u{3042}", Encoding::Windows1252),
        b"\xE9&#12354;"
    );
    assert_eq!(
        encoding::encode("\u{E9}", Encoding::Utf16Le),
        "\u{E9}".as_bytes()
    );
}

// ----- URL -----

fn href(input: &str, base: Option<&str>) -> Option<String> {
    let base = base.map(|b| Url::parse(b, None).expect("base parses"));
    Url::parse(input, base.as_ref()).ok().map(|u| u.href())
}

#[test]
fn ipv4_hosts_in_odd_forms() {
    assert_eq!(
        href("http://0x7f.1/", None).as_deref(),
        Some("http://127.0.0.1/")
    );
    assert_eq!(
        href("http://192.0x00A80001", None).as_deref(),
        Some("http://192.168.0.1/")
    );
    assert_eq!(
        href("http://4294967295", None).as_deref(),
        Some("http://255.255.255.255/")
    );
    assert_eq!(
        href("http://0xffffffff", None).as_deref(),
        Some("http://255.255.255.255/")
    );
    assert_eq!(
        href("http://10.0.0.1./", None).as_deref(),
        Some("http://10.0.0.1/")
    );
    assert_eq!(href("http://4294967296", None), None);
    assert_eq!(href("http://0xffffffff1", None), None);
    assert_eq!(href("http://1.2.3.09", None), None);
    assert_eq!(href("http://foo.09", None), None);
    assert_eq!(href("http://0..0x300/", None), None);
    assert_eq!(href("http://1.2.3.4.5.", None), None);
    // Non-special URLs keep numbers as opaque hosts.
    assert_eq!(href("sc://0x7f.1/", None).as_deref(), Some("sc://0x7f.1/"));
}

#[test]
fn ipv6_hosts_compress_the_first_longest_zero_run() {
    assert_eq!(
        href("http://[0:0:0:0:0:0:0:1]", None).as_deref(),
        Some("http://[::1]/")
    );
    assert_eq!(
        href("http://[1:0::]", None).as_deref(),
        Some("http://[1::]/")
    );
    assert_eq!(
        href("http://[::127.0.0.1]", None).as_deref(),
        Some("http://[::7f00:1]/")
    );
    assert_eq!(
        href("http://[0::0:0]", None).as_deref(),
        Some("http://[::]/")
    );
    assert_eq!(
        href("http://[1:0:0:2:0:0:0:3]", None).as_deref(),
        Some("http://[1:0:0:2::3]/")
    );
    assert_eq!(
        href("http://[1:0:0:2:0:0:3:4]", None).as_deref(),
        Some("http://[1::2:0:0:3:4]/")
    );
    assert_eq!(
        href("http://[ABCD::1]:8080/", None).as_deref(),
        Some("http://[abcd::1]:8080/")
    );
    assert_eq!(href("http://[1::2::3]", None), None);
    assert_eq!(href("http://[:1]", None), None);
    assert_eq!(href("http://[::1.2.3.4x]", None), None);
    assert_eq!(href("http://[1:2:3:4:5:6:7:8:9]", None), None);
}

#[test]
fn idna_hosts() {
    assert_eq!(
        href("https://faß.ExAmPlE/", None).as_deref(),
        Some("https://xn--fa-hia.example/")
    );
    assert_eq!(
        href("http://☃.net/", None).as_deref(),
        Some("http://xn--n3h.net/")
    );
    assert_eq!(
        href("http://Ｇｏ.com", None).as_deref(),
        Some("http://go.com/")
    );
    assert_eq!(
        href("http://www.foo。bar.com", None).as_deref(),
        Some("http://www.foo.bar.com/")
    );
    assert_eq!(
        href("http://GOO\u{200B}\u{2060}\u{FEFF}goo.com", None).as_deref(),
        Some("http://googoo.com/")
    );
    assert_eq!(
        href("http://a\u{AD}b/", None).as_deref(),
        Some("http://ab/")
    );
    assert_eq!(
        href("http://ex%61mple.com/", None).as_deref(),
        Some("http://example.com/")
    );
    assert_eq!(
        href("http://xn--n3h.net/", None).as_deref(),
        Some("http://xn--n3h.net/")
    );
    // Full-width space maps to a space, which is forbidden.
    assert_eq!(href("http://GOO\u{A0}\u{3000}goo.com", None), None);
    assert_eq!(href("http://\u{AD}/", None), None);
    assert_eq!(href("http://xn--/", None), None);
    assert_eq!(href("http://%zz%66%a.com", None), None);
    assert_eq!(href("http://％４１.com", None), None);
    // Non-special hosts are percent-encoded, not converted.
    assert_eq!(
        href("sc://ñ.test/", None).as_deref(),
        Some("sc://%C3%B1.test/")
    );
    assert_eq!(
        vidrio::url::domain_to_ascii("Bücher.example", false).as_deref(),
        Some("xn--bcher-kva.example")
    );
    assert_eq!(
        vidrio::url::domain_to_unicode("xn--bcher-kva.example", false),
        "bücher.example"
    );
}

#[test]
fn percent_encoding_per_component() {
    assert_eq!(
        href("foo://joe: !\"$%&'()*+,-.:;<=>@[\\]^_`{|}~@host/", None).as_deref(),
        Some("foo://joe:%20!%22$%&'()*+,-.%3A%3B%3C%3D%3E%40%5B%5C%5D%5E_%60%7B%7C%7D~@host/")
    );
    assert_eq!(
        href("foo://host/ !\"$%&'()*+,-./:;<=>@[\\]^_`{|}~", None).as_deref(),
        Some("foo://host/%20!%22$%&'()*+,-./:;%3C=%3E@[\\]%5E_%60%7B|%7D~")
    );
    assert_eq!(
        href("wss://host/dir/? !\"$%&'()*+,-./:;<=>?@[\\]^_`{|}~", None).as_deref(),
        Some("wss://host/dir/?%20!%22$%&%27()*+,-./:;%3C=%3E?@[\\]^_`{|}~")
    );
    assert_eq!(
        href("foo://host/dir/? !\"$%&'()*+,-./:;<=>?@[\\]^_`{|}~", None).as_deref(),
        Some("foo://host/dir/?%20!%22$%&'()*+,-./:;%3C=%3E?@[\\]^_`{|}~")
    );
    assert_eq!(
        href("foo://host/dir/# !\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~", None).as_deref(),
        Some("foo://host/dir/#%20!%22#$%&'()*+,-./:;%3C=%3E?@[\\]^_%60{|}~")
    );
    assert_eq!(
        href("https://x/\u{0}y", None).as_deref(),
        Some("https://x/%00y")
    );
    assert_eq!(
        href("http://h/\u{E9}?\u{E9}#\u{E9}", None).as_deref(),
        Some("http://h/%C3%A9?%C3%A9#%C3%A9")
    );
    assert_eq!(
        href("non-special:opaque  ?hi", None).as_deref(),
        Some("non-special:opaque %20?hi")
    );
}

#[test]
fn query_uses_the_document_encoding_except_for_ws() {
    let parse = |input: &str| {
        Url::parse_with_encoding(input, None, Encoding::Windows1252)
            .unwrap()
            .href()
    };
    assert_eq!(
        parse("http://h/\u{E9}?\u{E9}\u{2603}"),
        "http://h/%C3%A9?%E9%26%239731%3B"
    );
    assert_eq!(parse("wss://h/?\u{E9}"), "wss://h/?%C3%A9");
    assert_eq!(parse("sc://h/?\u{E9}"), "sc://h/?%C3%A9");
}

#[test]
fn relative_and_setters() {
    let base = Some("http://user:pass@example.org:81/a/b/c?q#f");
    assert_eq!(
        href("../../d", base).as_deref(),
        Some("http://user:pass@example.org:81/d")
    );
    assert_eq!(
        href("/./x/%2e%2E/y", base).as_deref(),
        Some("http://user:pass@example.org:81/y")
    );
    assert_eq!(
        href("#g", base).as_deref(),
        Some("http://user:pass@example.org:81/a/b/c?q#g")
    );
    assert_eq!(
        href("\\\\other\\p", base).as_deref(),
        Some("http://other/p")
    );
    assert_eq!(href("mailto:x", base).as_deref(), Some("mailto:x"));
    assert_eq!(href("x", Some("mailto:y")), None);

    let mut url = Url::parse("http://example.com/", None).unwrap();
    url.set_host("example.net:65536");
    assert_eq!(url.href(), "http://example.net/");
    url.set_protocol("https");
    url.set_port("443");
    assert_eq!(url.href(), "https://example.net/");
    url.set_protocol("mailto");
    assert_eq!(url.scheme, "https");
    url.set_pathname("a b");
    url.set_search("?x y");
    url.set_hash("z`");
    assert_eq!(url.href(), "https://example.net/a%20b?x%20y#z%60");
    assert_eq!(url.origin().serialize(), "https://example.net");
}

#[test]
fn form_urlencoded_round_trip() {
    let pairs = form::parse(b"a=b+c&%E2%98%83=%zz&&d");
    assert_eq!(
        pairs,
        vec![
            ("a".to_string(), "b c".to_string()),
            ("\u{2603}".to_string(), "%zz".to_string()),
            ("d".to_string(), String::new()),
        ]
    );
    let refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    assert_eq!(form::serialize(refs), "a=b+c&%E2%98%83=%25zz&d=");
}

// ----- MIME types -----

fn mime(input: &str) -> Option<String> {
    MimeType::parse(input).map(|m| m.serialize())
}

#[test]
fn mime_types_with_quoted_parameters() {
    assert_eq!(
        mime("text/html;charset=\"gbk\"").as_deref(),
        Some("text/html;charset=gbk")
    );
    assert_eq!(
        mime("text/html;charset=\"gbk").as_deref(),
        Some("text/html;charset=gbk")
    );
    assert_eq!(
        mime("text/html;charset=\"gbk\"x").as_deref(),
        Some("text/html;charset=gbk")
    );
    assert_eq!(
        mime("text/html;charset=gbk\"").as_deref(),
        Some("text/html;charset=\"gbk\\\"\"")
    );
    assert_eq!(
        mime("text/html;charset= \"gbk\"").as_deref(),
        Some("text/html;charset=\" \\\"gbk\\\"\"")
    );
    assert_eq!(
        mime("text/html;charset=\"\\ gbk\"").as_deref(),
        Some("text/html;charset=\" gbk\"")
    );
    assert_eq!(
        mime("text/html;charset=\"\\g\\b\\k\"").as_deref(),
        Some("text/html;charset=gbk")
    );
    assert_eq!(
        mime("text/html;charset=\"\";charset=GBK").as_deref(),
        Some("text/html;charset=\"\"")
    );
    assert_eq!(
        mime("text/html;charset=\";charset=GBK").as_deref(),
        Some("text/html;charset=\";charset=GBK\"")
    );
    assert_eq!(
        mime("text/html;valid=\";\";foo=bar").as_deref(),
        Some("text/html;valid=\";\";foo=bar")
    );
    assert_eq!(
        mime("text/html;in]valid=\";asd=foo\";foo=bar").as_deref(),
        Some("text/html;foo=bar")
    );
    assert_eq!(
        mime("text/html;charset= \"\u{7F};charset=GBK").as_deref(),
        Some("text/html;charset=GBK")
    );
    assert_eq!(mime("x/x;test=\"\\").as_deref(), Some("x/x;test=\"\\\\\""));
    assert_eq!(mime("\"text/html\""), None);

    let m = MimeType::parse(" TEXT/JavaScript ;CHARSET=UTF-8").unwrap();
    assert_eq!(m.essence(), "text/javascript");
    assert_eq!(m.parameter("charset"), Some("UTF-8"));
    assert!(m.is_javascript());
    assert!(MimeType::parse("image/svg+xml").unwrap().is_xml());
    assert!(MimeType::parse("application/ld+json").unwrap().is_json());
    assert_eq!(
        MimeType::parse_bytes(b"text/plain;x=\xE9")
            .unwrap()
            .parameter("x"),
        Some("\u{E9}")
    );
}
