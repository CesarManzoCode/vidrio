//! TLS 1.3 and its cryptography: official vectors of every primitive
//! (FIPS 180-4, FIPS 197, RFC 4231, RFC 5869, NIST GCM, RFC 8439, RFC 7748,
//! RFC 6979), the RFC 8448 §3 handshake trace replayed with its fixed keys,
//! and an X.509 chain generated with the fixed keys kept below.
//!
//! The RFC values were copied from the RFC text by script, not by hand.

use std::sync::Arc;

use vnet::crypto::aes::{Aes, AesGcm};
use vnet::crypto::bigint::BigUint;
use vnet::crypto::chacha::{ChaCha20Poly1305, chacha20_block, chacha20_xor, poly1305};
use vnet::crypto::hmac::{hkdf_expand, hkdf_extract, hmac};
use vnet::crypto::p256::{self, EcdhPrivate};
use vnet::crypto::sha2::{Sha256, Sha512};
use vnet::crypto::x25519::{x25519, x25519_public};
use vnet::crypto::{Aead, HashAlg, random};
use vnet::tls::key_schedule::{
    RecordCipher, derive_secret, early_secret, handshake_secret, hkdf_expand_label, master_secret,
    next_traffic_secret, traffic_keys,
};
use vnet::tls::{
    CipherSuite, ClientConfig, ClientConnection, KeyShare, NamedGroup, ServerVerifier, TlsError,
    alert,
};
use vnet::x509::{
    CertError, Certificate, PublicKey, TrustStore, days_from_civil, dns_name_matches,
    pem_certificates,
};

fn hex(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    assert!(s.len() % 2 == 0, "odd hex length");
    s.chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn arr32(s: &str) -> [u8; 32] {
    hex(s).try_into().unwrap()
}

fn arr12(s: &str) -> [u8; 12] {
    hex(s).try_into().unwrap()
}

// ---------------------------------------------------------------- SHA-2

const ABC: &[u8] = b"abc";
const MSG_448: &[u8] = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
const MSG_896: &[u8] = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";

#[test]
fn sha256_fips_180_4() {
    let cases: [(&[u8], &str); 4] = [
        (
            b"",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            ABC,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            MSG_448,
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
        (
            MSG_896,
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
        ),
    ];
    for (msg, want) in cases {
        assert_eq!(to_hex(&Sha256::digest(msg)), want);
        assert_eq!(to_hex(&HashAlg::Sha256.digest(msg)), want);
    }
    // One million 'a', fed in uneven pieces.
    let mut h = Sha256::new();
    let block = [b'a'; 997];
    let mut left = 1_000_000;
    while left > 0 {
        let n = left.min(block.len());
        h.update(&block[..n]);
        left -= n;
    }
    assert_eq!(
        to_hex(&h.finish()),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

#[test]
fn sha384_and_sha512_fips_180_4() {
    let cases: [(&[u8], &str, &str); 4] = [
        (
            b"",
            "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b",
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
        ),
        (
            ABC,
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7",
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        ),
        (
            MSG_448,
            "3391fdddfc8dc7393707a65b1b4709397cf8b1d162af05abfe8f450de5f36bc6b0455a8520bc4e6f5fe95b1fe3c8452b",
            "204a8fc6dda82f0a0ced7beb8e08a41657c16ef468b228a8279be331a703c33596fd15c13b1b07f9aa1d3bea57789ca031ad85c7a71dd70354ec631238ca3445",
        ),
        (
            MSG_896,
            "09330c33f71147e83d192fc782cd1b4753111b173b3b05d22fa08086e3b0f712fcc7c71a557e2db966c3e9fa91746039",
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909",
        ),
    ];
    for (msg, want384, want512) in cases {
        assert_eq!(to_hex(&HashAlg::Sha384.digest(msg)), want384);
        assert_eq!(to_hex(&HashAlg::Sha512.digest(msg)), want512);
    }
    let mut h384 = Sha512::new384();
    let mut h512 = Sha512::new();
    for _ in 0..1000 {
        h384.update(&[b'a'; 1000]);
        h512.update(&[b'a'; 1000]);
    }
    assert_eq!(
        to_hex(&h384.finish()),
        "9d0e1809716474cb086e834e310a4a1ced149e9c00f248527972cec5704c2a5b07b8b3dc38ecc4ebae97ddd87f3d8985"
    );
    assert_eq!(
        to_hex(&h512.finish()),
        "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973ebde0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b"
    );
}

#[test]
fn hasher_current_does_not_disturb_the_running_hash() {
    let mut h = HashAlg::Sha384.hasher();
    h.update(b"ab");
    let mid = h.current();
    h.update(b"c");
    assert_eq!(mid, HashAlg::Sha384.digest(b"ab"));
    assert_eq!(h.finish(), HashAlg::Sha384.digest(ABC));
}

// ---------------------------------------------------------------- HMAC and HKDF

/// RFC 4231 test cases 1-7: (key, data, HMAC-SHA-256, -384, -512).
/// Test case 5 lists only the first 128 bits of each output.
const RFC4231: [(&str, &str, &str, &str, &str); 7] = [
    (
        "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
        "4869205468657265",
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4af152e8b2fa9cb6",
        "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cdedaa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854",
    ),
    (
        "4a656665",
        "7768617420646f2079612077616e7420666f72206e6f7468696e673f",
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
        "af45d2e376484031617f78d2b58a6b1b9c7ef464f5a01b47e42ec3736322445e8e2240ca5e69e2c78b3239ecfab21649",
        "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea2505549758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737",
    ),
    (
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
        "88062608d3e6ad8a0aa2ace014c8a86f0aa635d947ac9febe83ef4e55966144b2a5ab39dc13814b94e3ab6e101a34f27",
        "fa73b0089d56a284efb0f0756c890be9b1b5dbdd8ee81a3655f83e33b2279d39bf3e848279a722c806b485a47e67c807b946a337bee8942674278859e13292fb",
    ),
    (
        "0102030405060708090a0b0c0d0e0f10111213141516171819",
        "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
        "3e8a69b7783c25851933ab6290af6ca77a9981480850009cc5577c6e1f573b4e6801dd23c4a7d679ccf8a386c674cffb",
        "b0ba465637458c6990e5a8c5f61d4af7e576d97ff94b872de76f8050361ee3dba91ca5c11aa25eb4d679275cc5788063a5f19741120c4f2de2adebeb10a298dd",
    ),
    (
        "0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c",
        "546573742057697468205472756e636174696f6e",
        "a3b6167473100ee06e0c796c2955552b",
        "3abf34c3503b2a23a46efc619baef897",
        "415fad6271580a531d4179bc891d87a6",
    ),
    (
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "54657374205573696e67204c6172676572205468616e20426c6f636b2d53697a65204b6579202d2048617368204b6579204669727374",
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
        "4ece084485813e9088d2c63a041bc5b44f9ef1012a2b588f3cd11f05033ac4c60c2ef6ab4030fe8296248df163f44952",
        "80b24263c7c1a3ebb71493c1dd7be8b49b46d1f41b4aeec1121b013783f8f3526b56d037e05f2598bd0fd2215d6a1e5295e64f73f63f0aec8b915a985d786598",
    ),
    (
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "5468697320697320612074657374207573696e672061206c6172676572207468616e20626c6f636b2d73697a65206b657920616e642061206c6172676572207468616e20626c6f636b2d73697a6520646174612e20546865206b6579206e6565647320746f20626520686173686564206265666f7265206265696e6720757365642062792074686520484d414320616c676f726974686d2e",
        "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
        "6617178e941f020d351e2f254e8fd32c602420feb0b8fb9adccebb82461e99c5a678cc31e799176d3860e6110c46523e",
        "e37b6a775dc87dbaa4dfa9f96e5e3ffddebd71f8867289865df5a32d20cdc944b6022cac3c4982b10d5eeb55c3e4de15134676fb6de0446065c97440fa8c6a58",
    ),
];

#[test]
fn hmac_rfc_4231() {
    for (i, (key, data, s256, s384, s512)) in RFC4231.iter().enumerate() {
        for (alg, want) in [
            (HashAlg::Sha256, s256),
            (HashAlg::Sha384, s384),
            (HashAlg::Sha512, s512),
        ] {
            let got = to_hex(&hmac(alg, &hex(key), &hex(data)));
            assert!(got.starts_with(want), "case {} {alg:?}: {got}", i + 1);
            if i != 4 {
                assert_eq!(&got, want, "case {} {alg:?}", i + 1);
            }
        }
    }
}

/// RFC 5869 A.1-A.3 (SHA-256): IKM, salt, info, L, PRK, OKM.
const RFC5869: [(&str, &str, &str, usize, &str, &str); 3] = [
    (
        "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
        "000102030405060708090a0b0c",
        "f0f1f2f3f4f5f6f7f8f9",
        42,
        "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5",
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865",
    ),
    (
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f",
        "606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9fa0a1a2a3a4a5a6a7a8a9aaabacadaeaf",
        "b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedfe0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
        82,
        "06a6b88c5853361a06104c9ceb35b45cef760014904671014a193f40c15fc244",
        "b11e398dc80327a1c8e7f78c596a49344f012eda2d4efad8a050cc4c19afa97c59045a99cac7827271cb41c65e590e09da3275600c2f09b8367793a9aca3db71cc30c58179ec3e87c14c01d5c1f3434f1d87",
    ),
    (
        "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
        "",
        "",
        42,
        "19ef24a32c717b167f33a91d6f648bdf96596776afdb6377ac434c1c293ccb04",
        "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8",
    ),
];

#[test]
fn hkdf_rfc_5869() {
    for (ikm, salt, info, len, prk, okm) in RFC5869 {
        let got_prk = hkdf_extract(HashAlg::Sha256, &hex(salt), &hex(ikm));
        assert_eq!(to_hex(&got_prk), prk);
        assert_eq!(
            to_hex(&hkdf_expand(HashAlg::Sha256, &got_prk, &hex(info), len)),
            okm
        );
    }
}

// ---------------------------------------------------------------- AES and GCM

#[test]
fn aes_fips_197_appendix_c() {
    let mut block: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
    Aes::new(&hex("000102030405060708090a0b0c0d0e0f")).encrypt_block(&mut block);
    assert_eq!(to_hex(&block), "69c4e0d86a7b0430d8cdb78070b4c55a");
    let mut block: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
    Aes::new(&hex(
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
    ))
    .encrypt_block(&mut block);
    assert_eq!(to_hex(&block), "8ea2b7ca516745bfeafc49904b496089");
}

const GCM_K: &str = "feffe9928665731c6d6a8f9467308308";
const GCM_K256: &str = "feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308";
const GCM_IV: &str = "cafebabefacedbaddecaf888";
const GCM_P: &str = "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72\
                     1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255";
const GCM_A: &str = "feedfacedeadbeeffeedfacedeadbeefabaddad2";
const ZERO16: &str = "00000000000000000000000000000000";
const ZERO32: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const ZERO_IV: &str = "000000000000000000000000";

/// The GCM specification's test cases 1-4 (AES-128) and 13-16 (AES-256),
/// the ones with 96-bit IVs: key, IV, plaintext, AAD, ciphertext, tag.
const GCM_CASES: [(&str, &str, &str, &str, &str, &str); 8] = [
    (
        ZERO16,
        ZERO_IV,
        "",
        "",
        "",
        "58e2fccefa7e3061367f1d57a4e7455a",
    ),
    (
        ZERO16,
        ZERO_IV,
        ZERO16,
        "",
        "0388dace60b6a392f328c2b971b2fe78",
        "ab6e47d42cec13bdf53a67b21257bddf",
    ),
    (
        GCM_K,
        GCM_IV,
        GCM_P,
        "",
        "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e\
         21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091473f5985",
        "4d5c2af327cd64a62cf35abd2ba6fab4",
    ),
    (
        GCM_K,
        GCM_IV,
        "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72\
         1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
        GCM_A,
        "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e\
         21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091",
        "5bc94fbc3221a5db94fae95ae7121a47",
    ),
    (
        ZERO32,
        ZERO_IV,
        "",
        "",
        "",
        "530f8afbc74536b9a963b4f1c4cb738b",
    ),
    (
        ZERO32,
        ZERO_IV,
        ZERO16,
        "",
        "cea7403d4d606b6e074ec5d3baf39d18",
        "d0d1c8a799996bf0265b98b5d48ab919",
    ),
    (
        GCM_K256,
        GCM_IV,
        GCM_P,
        "",
        "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa\
         8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662898015ad",
        "b094dac5d93471bdec1a502270e3cc6c",
    ),
    (
        GCM_K256,
        GCM_IV,
        "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72\
         1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
        GCM_A,
        "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa\
         8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662",
        "76fc6ece0f4e1768cddf8853bb2d551b",
    ),
];

#[test]
fn aes_gcm_nist_vectors() {
    for (i, (key, iv, pt, aad, ct, tag)) in GCM_CASES.iter().enumerate() {
        let gcm = AesGcm::new(&hex(key));
        let nonce = arr12(iv);
        let sealed = gcm.seal(&nonce, &hex(aad), &hex(pt));
        assert_eq!(
            to_hex(&sealed),
            format!("{ct}{tag}").replace(' ', ""),
            "case {i}"
        );
        assert_eq!(
            gcm.open(&nonce, &hex(aad), &sealed),
            Some(hex(pt)),
            "case {i}"
        );
        let mut bad = sealed.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert_eq!(gcm.open(&nonce, &hex(aad), &bad), None, "case {i}");
    }
}

// ---------------------------------------------------------------- ChaCha20-Poly1305

const KEY_0_31: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const CHACHA_BLOCK: &str = "\
    10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4e\
    d2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e";
const SUNSCREEN: &str = "\
    4c616469657320616e642047656e746c656d656e206f662074686520636c6173\
    73206f66202739393a204966204920636f756c64206f6666657220796f75206f\
    6e6c79206f6e652074697020666f7220746865206675747572652c2073756e73\
    637265656e20776f756c642062652069742e";
const SUNSCREEN_CT: &str = "\
    6e2e359a2568f98041ba0728dd0d6981e97e7aec1d4360c20a27afccfd9fae0b\
    f91b65c5524733ab8f593dabcd62b3571639d624e65152ab8f530c359f0861d8\
    07ca0dbf500d6a6156a38e088a22b65e52bc514d16ccf806818ce91ab7793736\
    5af90bbf74a35be6b40b8eedf2785e42874d";
const POLY_MSG: &str = "\
    43727970746f6772617068696320466f72756d2052657365617263682047726f\
    7570";
const AEAD_CT: &str = "\
    d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6\
    3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36\
    92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc\
    3ff4def08e4b7a9de576d26586cec64b6116";

#[test]
fn chacha20_rfc_8439() {
    let key = arr32(KEY_0_31);
    // §2.3.2: the block function.
    let block = chacha20_block(&key, 1, &arr12("000000090000004a00000000"));
    assert_eq!(block.to_vec(), hex(CHACHA_BLOCK));
    // §2.4.2: encryption from counter 1.
    let mut data = hex(SUNSCREEN);
    chacha20_xor(&key, 1, &arr12("000000000000004a00000000"), &mut data);
    assert_eq!(data, hex(SUNSCREEN_CT));
}

#[test]
fn poly1305_rfc_8439() {
    // §2.5.2.
    let key = arr32("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b");
    assert_eq!(
        to_hex(&poly1305(&key, &hex(POLY_MSG))),
        "a8061dc1305136c6c22b8baf0c0127a9"
    );
}

#[test]
fn chacha20_poly1305_rfc_8439() {
    // §2.8.2.
    let aead = ChaCha20Poly1305::new(&arr32(
        "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f",
    ));
    let nonce = arr12("070000004041424344454647");
    let aad = hex("50515253c0c1c2c3c4c5c6c7");
    let sealed = aead.seal(&nonce, &aad, &hex(SUNSCREEN));
    let mut want = hex(AEAD_CT);
    want.extend(hex("1ae10b594f09e26a7e902ecbd0600691"));
    assert_eq!(sealed, want);
    assert_eq!(aead.open(&nonce, &aad, &sealed), Some(hex(SUNSCREEN)));
    let mut bad = sealed.clone();
    bad[0] ^= 0x80;
    assert_eq!(aead.open(&nonce, &aad, &bad), None);
    assert_eq!(aead.open(&nonce, b"other aad", &sealed), None);
}

#[test]
fn aead_dispatch_matches_the_primitives() {
    let key = hex(KEY_0_31);
    let nonce = arr12("000000000000000000000001");
    let gcm = Aead::aes_gcm(&key);
    assert_eq!(
        gcm.seal(&nonce, b"a", b"hello"),
        AesGcm::new(&key).seal(&nonce, b"a", b"hello")
    );
    let chacha = Aead::chacha20_poly1305(&arr32(KEY_0_31));
    let sealed = chacha.seal(&nonce, b"a", b"hello");
    assert_eq!(sealed.len(), 5 + Aead::TAG_LEN);
    assert_eq!(chacha.open(&nonce, b"a", &sealed), Some(b"hello".to_vec()));
}

// ---------------------------------------------------------------- X25519

#[test]
fn x25519_rfc_7748_section_5_2() {
    let cases = [
        (
            "a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4",
            "e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c",
            "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552",
        ),
        (
            "4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d",
            "e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493",
            "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957",
        ),
    ];
    for (k, u, out) in cases {
        assert_eq!(to_hex(&x25519(&arr32(k), &arr32(u))), out);
    }
}

#[test]
fn x25519_rfc_7748_iterations() {
    let mut k = [0u8; 32];
    k[0] = 9;
    let mut u = k;
    for i in 1..=1000 {
        let next = x25519(&k, &u);
        u = k;
        k = next;
        if i == 1 {
            assert_eq!(
                to_hex(&k),
                "422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079"
            );
        }
    }
    assert_eq!(
        to_hex(&k),
        "684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51"
    );
}

#[test]
fn x25519_rfc_7748_diffie_hellman() {
    let a = arr32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let b = arr32("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
    let pa = x25519_public(&a);
    let pb = x25519_public(&b);
    assert_eq!(
        to_hex(&pa),
        "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
    );
    assert_eq!(
        to_hex(&pb),
        "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f"
    );
    let shared = "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742";
    assert_eq!(to_hex(&x25519(&a, &pb)), shared);
    assert_eq!(to_hex(&x25519(&b, &pa)), shared);
}

#[test]
fn x25519_share_rejects_small_order_points() {
    let share = KeyShare::x25519([7; 32]);
    assert_eq!(share.group(), NamedGroup::X25519);
    // u = 0 has small order: the shared secret is all zeros.
    assert_eq!(share.agree(&[0u8; 32]), None);
    assert_eq!(share.agree(&[9u8; 31]), None);
}

// ---------------------------------------------------------------- P-256

/// RFC 6979 A.2.5 key pair.
const P256_X: &str = "c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721";
const P256_UX: &str = "60fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6";
const P256_UY: &str = "7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299";

fn p256_public() -> Vec<u8> {
    let mut q = vec![4u8];
    q.extend(hex(P256_UX));
    q.extend(hex(P256_UY));
    q
}

#[test]
fn p256_public_key_rfc_6979() {
    let k = EcdhPrivate::from_bytes(&hex(P256_X)).unwrap();
    assert_eq!(k.public_key(), p256_public());
    let share = KeyShare::p256(&hex(P256_X)).unwrap();
    assert_eq!(share.group(), NamedGroup::Secp256r1);
    assert_eq!(share.public(), &p256_public()[..]);
}

#[test]
fn p256_ecdh_agrees() {
    let a = EcdhPrivate::from_bytes(&hex(P256_X)).unwrap();
    let b = EcdhPrivate::from_bytes(&hex(
        "0000000000000000000000000000000000000000000000000000000000000003",
    ))
    .unwrap();
    let ab = a.agree(&b.public_key()).unwrap();
    let ba = b.agree(&a.public_key()).unwrap();
    assert_eq!(ab, ba);
    assert_eq!(ab.len(), 32);
    // Off-curve and malformed points are refused.
    let mut off = p256_public();
    off[64] ^= 1;
    assert_eq!(a.agree(&off), None);
    assert_eq!(a.agree(&p256_public()[..64]), None);
    // Out-of-range scalars are refused.
    assert!(EcdhPrivate::from_bytes(&[0u8; 32]).is_none());
    assert!(EcdhPrivate::from_bytes(&[0xffu8; 32]).is_none());
    let fresh = EcdhPrivate::generate().unwrap();
    assert_eq!(fresh.public_key().len(), 65);
}

#[test]
fn ecdsa_p256_rfc_6979() {
    // (hash, message, r, s)
    let cases = [
        (
            HashAlg::Sha256,
            "sample",
            "efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716",
            "f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8",
        ),
        (
            HashAlg::Sha384,
            "sample",
            "0eafea039b20e9b42309fb1d89e213057cbf973dc0cfc8f129edddc800ef7719",
            "4861f0491e6998b9455193e34e7b0d284ddd7149a74b95b9261f13abde940954",
        ),
        (
            HashAlg::Sha512,
            "sample",
            "8496a60b5e9b47c825488827e0495b0e3fa109ec4568fd3f8d1097678eb97f00",
            "2362ab1adbe2b8adf9cb9edab740ea6049c028114f2460f96554f61fae3302fe",
        ),
        (
            HashAlg::Sha256,
            "test",
            "f1abb023518351cd71d881567b1ea663ed3efcf6c5132b354f28d3b0b7d38367",
            "019f4113742a2b14bd25926b49c649155f267e60d3814b4c0cc84250e46f0083",
        ),
        (
            HashAlg::Sha384,
            "test",
            "83910e8b48bb0c74244ebdf7f07a1c5413d61472bd941ef3920e623fbccebeb6",
            "8ddbec54cf8cd5874883841d712142a56a8d0f218f5003cb0296b6b509619f2c",
        ),
    ];
    let q = p256_public();
    for (alg, msg, r, s) in cases {
        let digest = alg.digest(msg.as_bytes());
        let r = BigUint::from_be_bytes(&hex(r));
        let s = BigUint::from_be_bytes(&hex(s));
        assert!(p256::verify_raw(&q, &digest, &r, &s), "{alg:?} {msg}");
        let other = alg.digest(b"other");
        assert!(!p256::verify_raw(&q, &other, &r, &s), "{alg:?} {msg} other");
        assert!(
            !p256::verify_raw(&q, &digest, &s, &r),
            "{alg:?} {msg} swapped"
        );
    }
    // The same signature in DER form.
    let mut der = vec![0x30, 0x45, 0x02, 0x21, 0x00];
    der.extend(hex(
        "efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716",
    ));
    der.extend([0x02, 0x21, 0x00]);
    der.extend(hex(
        "f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8",
    ));
    assert!(p256::verify_der(
        &q,
        &HashAlg::Sha256.digest(b"sample"),
        &der
    ));
    assert!(!p256::verify_der(
        &q,
        &HashAlg::Sha256.digest(b"test"),
        &der
    ));
}

// ---------------------------------------------------------------- X.509 test chain
//
// Generated once with OpenSSL 3.6.5 from the keys below (validity
// 2025-01-01 to 2045-01-01):
//   root     RSA 2048, self-signed, sha256WithRSAEncryption, CA:TRUE (like the WPT CA)
//   inter    P-256, signed by root (sha256WithRSAEncryption), CA:TRUE, pathlen:0
//   leaf     P-256, signed by inter (ecdsa-with-SHA256), SAN example.test,
//            *.wild.example.test and 127.0.0.1, EKU serverAuth
//   rsaleaf  RSA 2048, signed by root, SAN web-platform.test and *.web-platform.test
//   bad      P-256, signed by rsaleaf, which is not a CA
// The SIG_* values sign MSG with the rsaleaf key (`openssl dgst -sign`, PSS
// with a salt as long as the hash).

const MSG: &[u8] = b"vidrio signature vector";

const ROOT_PEM: &str = "\
-----BEGIN CERTIFICATE-----\n\
MIIC8zCCAdugAwIBAgIBATANBgkqhkiG9w0BAQsFADAbMRkwFwYDVQQDDBB2aWRy\n\
aW8gdGVzdCByb290MB4XDTI1MDEwMTAwMDAwMFoXDTQ1MDEwMTAwMDAwMFowGzEZ\n\
MBcGA1UEAwwQdmlkcmlvIHRlc3Qgcm9vdDCCASIwDQYJKoZIhvcNAQEBBQADggEP\n\
ADCCAQoCggEBAKg7jxOgK5MY89ZnzjawKKN1neLKMMG9ojpFzyAhs8ybQ7OVzyu/\n\
TS1M8c89Y5V95Jj9duEksiqIkzaj6rn5cqGaPBHgJPI41PHN68JrbJJF0jrtlSIF\n\
sVCEqzTa3MeFGyUdOdJIICC7vpBz2Tgz7+3g3Zwz6QiFbojMwa2e3zu12s76N5z9\n\
czLEhbIwq3/6FEQwLdS+Bx9JI7KIGFe+8pgww38UTXWgEURD45iJjhaodxVQKdBs\n\
eFLinueVx2Eldw/dBZc2s/EG55yD+XffmBOU6pync32nmwjaIt58oNgLcrRgXsyB\n\
n8D4MsxRz5CCz9GNVAQIW9xgmmlmt3Rg530CAwEAAaNCMEAwDwYDVR0TAQH/BAUw\n\
AwEB/zAOBgNVHQ8BAf8EBAMCAQYwHQYDVR0OBBYEFPsXc6HsTnO4lCUg7tCNr4EO\n\
6cLPMA0GCSqGSIb3DQEBCwUAA4IBAQBfyOufoSmp9LLtNKu3A1mIor3GGjYkwUxf\n\
n6DHjTfu49ZMv0ll2YZ+A+IY14KSx3FuEKXic1FcHZ4hkhOZEMLo2HjpE0mkRJCY\n\
Le9b1gUN6EgmMSiGgc8KbQr/aNWiyV39iFXT7V2hljOZgOWS8WL0oQea3klcg+J8\n\
7G2mj5A0vCD1EalGeY0tEy87nxE/irdAjjJPv/mKr1FaNOrVnSXdoUJTE2U0zren\n\
1uDlTMkbX2zifDdnc/i0Fyg6lldqxz7Gu+GLRIsk1sWWaTJL0vuAgSGRQqESwy58\n\
JKNzyEVa0QiNkKwPRoi728iFukdkPftlMAYY3tRYfreoMHzas4HN\n\
-----END CERTIFICATE-----";

const INTER_PEM: &str = "\
-----BEGIN CERTIFICATE-----\n\
MIICTTCCATWgAwIBAgIBAzANBgkqhkiG9w0BAQsFADAbMRkwFwYDVQQDDBB2aWRy\n\
aW8gdGVzdCByb290MB4XDTI1MDEwMTAwMDAwMFoXDTQ1MDEwMTAwMDAwMFowHDEa\n\
MBgGA1UEAwwRdmlkcmlvIHRlc3QgaW50ZXIwWTATBgcqhkjOPQIBBggqhkjOPQMB\n\
BwNCAATmiQogW/0FHH+vhJDvOekbecMe5xyiJkvjvhXUW5S+prUCzg/C1GkjK83c\n\
fPYHWV+ZFA5OznuHVC5pgM23PNroo2YwZDASBgNVHRMBAf8ECDAGAQH/AgEAMA4G\n\
A1UdDwEB/wQEAwIBBjAdBgNVHQ4EFgQUQBNxH576IPiIt9bN1dokiZ4rmMgwHwYD\n\
VR0jBBgwFoAU+xdzoexOc7iUJSDu0I2vgQ7pws8wDQYJKoZIhvcNAQELBQADggEB\n\
AF8g+Qo4/bRxqCZ4a6LbVkzExumQ3TYMwcaLfEiJiSdbKWZw/4RGyOLRsB4G+MH9\n\
NBZFpxHWDxMpiYE98/2CeQ4Mw/TfrJ9wXhtSe9T5uR7gRvbkEJQmQ0NlQrZ1FTKt\n\
ZzFq+64/OwxHtqhhDVQW+TJ0wAQHjkHH0Hy84/OcC19PjdSytJMQk1t1ZgUmCUJv\n\
ZzDO7cUTj34FpUO8v96XceU1u74gKMxvRF85UO/fFKj1sut2TYJLP6/OkBLsZdry\n\
RVmBXnBmPVDqBr0JxoagM1OQuMX0GLqv3JUzPBJSm11aCiMLxqpE1vqYo5VS6tsj\n\
DqESnlv47LMDiHTSHRIQgTU=\n\
-----END CERTIFICATE-----";

const LEAF_PEM: &str = "\
-----BEGIN CERTIFICATE-----\n\
MIIBzjCCAXSgAwIBAgIBBDAKBggqhkjOPQQDAjAcMRowGAYDVQQDDBF2aWRyaW8g\n\
dGVzdCBpbnRlcjAeFw0yNTAxMDEwMDAwMDBaFw00NTAxMDEwMDAwMDBaMBsxGTAX\n\
BgNVBAMMEHZpZHJpbyB0ZXN0IGxlYWYwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNC\n\
AAR+xY8/AO/96ymMccwTR2OiSPXbeUPw3t64KAwoaZ9zYu/O8VIGTqqcCuCddufl\n\
mr4F30cdHjBg2MLAFaU6AD0qo4GnMIGkMAkGA1UdEwQCMAAwDgYDVR0PAQH/BAQD\n\
AgeAMBMGA1UdJQQMMAoGCCsGAQUFBwMBMDIGA1UdEQQrMCmCDGV4YW1wbGUudGVz\n\
dIITKi53aWxkLmV4YW1wbGUudGVzdIcEfwAAATAdBgNVHQ4EFgQUQZxVeCStBlxe\n\
g7EZMxPpTmvQ3AswHwYDVR0jBBgwFoAUQBNxH576IPiIt9bN1dokiZ4rmMgwCgYI\n\
KoZIzj0EAwIDSAAwRQIgLPtuKQvjY6GIOz1Xu+Poqo/8GTgc3/hY254dxDMguEkC\n\
IQDLJZYY1anFCBCCvBAWTKcYnPrfTlhEK9FpIRuWIYzU2Q==\n\
-----END CERTIFICATE-----";

const RSALEAF_PEM: &str = "\
-----BEGIN CERTIFICATE-----\n\
MIIDWDCCAkCgAwIBAgIBBTANBgkqhkiG9w0BAQsFADAbMRkwFwYDVQQDDBB2aWRy\n\
aW8gdGVzdCByb290MB4XDTI1MDEwMTAwMDAwMFoXDTQ1MDEwMTAwMDAwMFowHjEc\n\
MBoGA1UEAwwTdmlkcmlvIHRlc3QgcnNhbGVhZjCCASIwDQYJKoZIhvcNAQEBBQAD\n\
ggEPADCCAQoCggEBAJ9wjDRXPyrrDBCo8KgTGVyFOe81X3XWq+/myZ7NeNpXh2gc\n\
KsnWgWkeWm5uzKnZaFPrC7Jh7utAnr28wGdOrVMYwah2oeRypXdvb2QTlwm4hmYj\n\
33FFhIuMt5WkoOxbOYVIXCBf8lcRSaUCKkxTD5u0vGxu9q9y7rJzp3LBJP5lOtcm\n\
PkYDZxzm/bwY0WfLJZAE+F9PmYpUYhBnS5mYYkApZmpgkPUnrajRgmZToUxzk9jQ\n\
cdQ5pq0us3MBgwa/DgREQwPjx0+HCo0EOOpcnfDTrH8Cj10jwTFTnj+5owGFYi3h\n\
/7D23iZZ4xYQpVpQx8UyfRQt9o5WdMz5o+y+amMCAwEAAaOBozCBoDAJBgNVHRME\n\
AjAAMAsGA1UdDwQEAwIF4DATBgNVHSUEDDAKBggrBgEFBQcDATAxBgNVHREEKjAo\n\
ghF3ZWItcGxhdGZvcm0udGVzdIITKi53ZWItcGxhdGZvcm0udGVzdDAdBgNVHQ4E\n\
FgQUOVV9JyQ+fLhjZR2NorxH5tvNwg0wHwYDVR0jBBgwFoAU+xdzoexOc7iUJSDu\n\
0I2vgQ7pws8wDQYJKoZIhvcNAQELBQADggEBACZdmfUulF/bZDcT2PtmCSiFVwaz\n\
q9gRRYZPeboHQ4S6JqKcdpKz9FRkxq2Mk4ks2oZOmpxHWB5AXT8S7EqS8dmVt859\n\
6XKMkrhoTGihdW7MWgqJuBGhbaQP20mzVrIY1RwpSTboHu1MmDptAEin6HRZ2V3T\n\
Xj6kJXUsq8PxMzVpvOkfJNjRTT0xi0M+9QRe0pf6OcJGQbAoqtPBFz0PJHDiQ6zN\n\
fnVevY76bG1XN/Kzh3/sydi04CQf4aaCeZjjEfR/70QkoCkPYvGSW7zK+kyq1An+\n\
sh6Dvt+2rCcwfk3uvKmmqIaLIGFNEr0cw5qcElbYc4DyoFkClKsjwGTWQIQ=\n\
-----END CERTIFICATE-----";

const BAD_PEM: &str = "\
-----BEGIN CERTIFICATE-----\n\
MIICYjCCAUqgAwIBAgIBBjANBgkqhkiG9w0BAQsFADAeMRwwGgYDVQQDDBN2aWRy\n\
aW8gdGVzdCByc2FsZWFmMB4XDTI1MDEwMTAwMDAwMFoXDTQ1MDEwMTAwMDAwMFow\n\
GjEYMBYGA1UEAwwPdmlkcmlvIHRlc3QgYmFkMFkwEwYHKoZIzj0CAQYIKoZIzj0D\n\
AQcDQgAE8PB7cqOA0/kqX+u/ZveOqM/QVtJ5rV/2HeikteaeUm/nKWY7jYrnXO6V\n\
8QoNWe3+koMxZTlV4gqo05dMw66Vj6N6MHgwCQYDVR0TBAIwADAOBgNVHQ8BAf8E\n\
BAMCB4AwGwYDVR0RBBQwEoIQYmFkLmV4YW1wbGUudGVzdDAdBgNVHQ4EFgQUaheo\n\
W/6FtVXpWoOowyXK9RyaB6QwHwYDVR0jBBgwFoAUOVV9JyQ+fLhjZR2NorxH5tvN\n\
wg0wDQYJKoZIhvcNAQELBQADggEBAJpwTvjzOFbfovTK6z6iLiJZXZKCGKyYmzoD\n\
YuLYzo6afMf/Le5NQtqzuQ3UIraDXeMXrNWqi/B0FzewyWInqIYh2VuFbBvSr5PJ\n\
G4xaPj9xGnjddWB3ECFOku07tIFIC7mbO/tB+GeDJjsvDy7+uEGA2aYDsCl1y1Rv\n\
dyge8Oa5RJOnWr2iz7O+plJm3KtRDhadtjtVqav/p5DvDmc1S2n0b6U0T3dq24M+\n\
dM0nEpDOlXvQwGfxdOWHc6At0w4FDTnvLjwD1AkwnXIkWe6ce0RgxKwKOFpaigME\n\
/ob8C11DQtxM8g88AKx/GlzalC+FTsgI1w+sSGb/8YwR+UAT4+o=\n\
-----END CERTIFICATE-----";

#[allow(dead_code)]
const ROOT_KEY_PEM: &str = "\
-----BEGIN PRIVATE KEY-----\n\
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCoO48ToCuTGPPW\n\
Z842sCijdZ3iyjDBvaI6Rc8gIbPMm0Ozlc8rv00tTPHPPWOVfeSY/XbhJLIqiJM2\n\
o+q5+XKhmjwR4CTyONTxzevCa2ySRdI67ZUiBbFQhKs02tzHhRslHTnSSCAgu76Q\n\
c9k4M+/t4N2cM+kIhW6IzMGtnt87tdrO+jec/XMyxIWyMKt/+hREMC3UvgcfSSOy\n\
iBhXvvKYMMN/FE11oBFEQ+OYiY4WqHcVUCnQbHhS4p7nlcdhJXcP3QWXNrPxBuec\n\
g/l335gTlOqcp3N9p5sI2iLefKDYC3K0YF7MgZ/A+DLMUc+Qgs/RjVQECFvcYJpp\n\
Zrd0YOd9AgMBAAECggEAFGipSlG3h5BS4HNWZncHbxERNtadhJtrR1N5taWxCMTb\n\
OTmW2PFLvQ+lFCWX46ZSqaayGc0YWiWYalwHWUlkOSubVvXQtZvkZXM8QylCEhqr\n\
Tt/MpER5htGZbX+RohjJOr7paVO7pgzwqNiNBQ7z7/FEITOA9ToutyTT6CqqXTVX\n\
aXH9A/fJ7pDHAOHI0OdBPej2Ne88TZnJcpKPGad41yXALnBavfBKTTMTutJl34bs\n\
AtfyPAIBIkdJPdIEVZCaMFnZqsuyHW81pl/9H1V2K/qbyssMyUXwr+e8PAhuTTxX\n\
ewohHzkEzZNu9sGSGQAiAbR7sduxJ7pTRVqRWCPFVwKBgQDocoocUCX/kiuIOFRd\n\
4iVDDCN2CzxGmR44XzP41mGx6jIa5Na/c7gZpU03XlGbRq/+R3mvluWrFfqLVPxI\n\
rHjaLBqc9JfbxIlL44rBxmFXOfMrlnnKffoUwZ0JLr8K1+EQb3RxmxwwmaaZtr9j\n\
1Mu70QM0thuttvQXXLfUtpJFJwKBgQC5R1nC+qStPIghse0e9CvY3Gdzk48K7PoW\n\
aaBZZIcMJIyliZegUcJZ0BZgDBrqIxl5kXtRo5lohpS6ZLk0yi92XWQIPfx3cW9T\n\
QWWuRdD09VzIZbPU8LbIeAHEzPjeu7zRqpIunEbssgJi96RDj/st5UBCrivzicHf\n\
Qytgrpr8uwKBgH7Iyq/6v18QuRHJt6qliLFzU3w1UKEpLySMOOrG8+540O9+VeuD\n\
EK1cTiufFF/fRGsrTSIK6iZRDKEmr5xsBlbpq3Jkrte98S0lZS+gJsPO5eWmY7Og\n\
1uCvYhn6F1Wwvh+ayyaT9eJETGyXEvk+vgkpTdhYY+xyV9zOQ66QDmQNAoGAFT4j\n\
AoHuQ5Z/HS3jO/tO06xhiBTQ/7Wv7sXphpZKDZBbN9eQtObq25VKG3UH2PRxRUjy\n\
1KdnPV7AyqYlvfG4g1+tymwg+CjfVLJY0CRdEtjW7HsMpUt8CXwI5+GAXzkzKu7d\n\
l85/K0A1mdc+1Cg76kUwmvSRQXj363suDAW462UCgYEAupu8LTS1c4uhrZ+JRCbZ\n\
cTJBW8MD+j/kpRl1D2lmtbFyTTDU0q9XRGZwNvsdfq8BklANepPlHP0t0sOto1+6\n\
cew+zYkHGHGdbdgopFY7EESQeOkg6+nUUdnZz0PhIwHpYpDyvE4em34BBULsTk06\n\
ID9jKrL15d4ddR8YpV6RuPo=\n\
-----END PRIVATE KEY-----";

#[allow(dead_code)]
const INTER_KEY_PEM: &str = "\
-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgWeOqaKG3Ht+sCpYy\n\
uIayqGfMRgi8laPvWuY9rd9cpdWhRANCAATmiQogW/0FHH+vhJDvOekbecMe5xyi\n\
JkvjvhXUW5S+prUCzg/C1GkjK83cfPYHWV+ZFA5OznuHVC5pgM23PNro\n\
-----END PRIVATE KEY-----";

#[allow(dead_code)]
const LEAF_KEY_PEM: &str = "\
-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgEsUdQ3+reHDKpERU\n\
8cBaKnGj2gWfiMED8hSRvzKK2nahRANCAAR+xY8/AO/96ymMccwTR2OiSPXbeUPw\n\
3t64KAwoaZ9zYu/O8VIGTqqcCuCdduflmr4F30cdHjBg2MLAFaU6AD0q\n\
-----END PRIVATE KEY-----";

#[allow(dead_code)]
const RSALEAF_KEY_PEM: &str = "\
-----BEGIN PRIVATE KEY-----\n\
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQCfcIw0Vz8q6wwQ\n\
qPCoExlchTnvNV911qvv5smezXjaV4doHCrJ1oFpHlpubsyp2WhT6wuyYe7rQJ69\n\
vMBnTq1TGMGodqHkcqV3b29kE5cJuIZmI99xRYSLjLeVpKDsWzmFSFwgX/JXEUml\n\
AipMUw+btLxsbvavcu6yc6dywST+ZTrXJj5GA2cc5v28GNFnyyWQBPhfT5mKVGIQ\n\
Z0uZmGJAKWZqYJD1J62o0YJmU6FMc5PY0HHUOaatLrNzAYMGvw4EREMD48dPhwqN\n\
BDjqXJ3w06x/Ao9dI8ExU54/uaMBhWIt4f+w9t4mWeMWEKVaUMfFMn0ULfaOVnTM\n\
+aPsvmpjAgMBAAECggEAF6PDVftdpUpmZo316KW0S/CA74O1XK5wnchVOpNyiMUT\n\
kY8jlPuWAnT6qYcKF2DhxBeEi3VTzFOvdahnyqlyr3BrPkQgqtOIzxWZqXfyHAHd\n\
A4XsGkpZZat+iNR4+7Jm6+kXp8EyGFC635lblid2l/qLOXSWZNsgoxrcx/n1Nzak\n\
86Ki4elmoNiV1AVxu4c8EybC6dEbGSoBZwc8S79MYLf5ngwYuEwP2jouERZH9LDy\n\
3w4495VsUuDm1umsZ4LHtGpej5vRFpbUVBZa0NJ7ApANMFlUcx4v3MdGkbY+sLJ8\n\
EcLkg2vGmhssblrFmO2NQDjqozroSHKACiStYKSiKQKBgQDfn16e8OwbrlGrl1jC\n\
hmDdZTnX3yFJWkcsPu5S5/ne1DD6QznJe5BpdoT9E7vynPdvXybPNjyx/RCl3vdj\n\
cjhS8I62LLTb4MtuTHoGb78fOphNZQIXf+43CKVywCP01O2KGEXuHwouCxvtIz65\n\
A1aYvq3S2L2T1wfEXGSEUpLv/QKBgQC2hjivGVTt2JDghoLxX8u343r0pXxZ4DIG\n\
w5/SmTvfSdE2HhichXO/pO18Cr0rxmh5TtJ/In8AAjRLz7mhFxcSkMUKvyVquURE\n\
XY3+Vh5ohxkaOUKnU7icBanXD6WwGVR1TvBJyIFGwOixOa8stb0H6IGWEu52SA5C\n\
7jtnLLrh3wKBgAsbSEDOqHtregTyvGbU88SrPWc6XtXc1QIekgmuGdv2YAReCGW4\n\
bGDAkTRcoY4YO7XdXPkXsYYJkqUaYOEgkCmWZxAnQkhVbuUoiTr+m9h6OYxGgwcV\n\
xIgZNtZvMpn7ZVzhfb+o7QGCbb2mbXwU7V0iw1FgkHHiUTFwgVGNJF0lAoGAC0NF\n\
BCkAAJ04c2JZeiN7IqG62JXNko4vCBPSLOCfvSuE6V36Ds59UfoPXXQcMDNSBwpC\n\
Gu45//oi1/q9bZ9LIS4luoAJAwjjmYRcWrFdtv1/Iwsdfc3Aun/u9wJUKwWlHIf+\n\
c88XixIkSatRj+S3AWHrfSM1676OgrrIl5k+/ukCgYBvvz+RIpslVGr0J+FdbDqH\n\
Tcr3cBB30LO+1IO949Y0rQUyW7xKCt/4KI3WJtESFEEXTwAA7micwYsgU7CCTPca\n\
AdttMja+U7Cdb9QfBCbId6/6ZtcP+y678WGWP7kS33ZrXhQlwTZSpf5+xswEqHzY\n\
umwmMlQb1HancyhaYD1s2A==\n\
-----END PRIVATE KEY-----";

#[allow(dead_code)]
const BAD_KEY_PEM: &str = "\
-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgK3A8Gl14dOMjhkme\n\
RDv+eHJCYsBL1DMQLfP+rrcbpSShRANCAATw8Htyo4DT+Spf679m946oz9BW0nmt\n\
X/Yd6KS15p5Sb+cpZjuNiudc7pXxCg1Z7f6SgzFlOVXiCqjTl0zDrpWP\n\
-----END PRIVATE KEY-----";

const SIG_PSS_SHA256: &str = "\
    4e4b5c0cca231826d07f58283eabf0bfba6e966ff38c795efef3e9061b116964\
    6dfef5cddf833bf84577e2bd00620df225a57290d2b6c993f11d491d737041b0\
    61117d1ca98934ae88640b2d9cded01febba77992f3dccce08af0e9b99c3dc5b\
    b786242d4162f0a89112948f735057b1047ff23d0ced61e440f07c702274e1a4\
    cf093f08afe9469b6378af21b53ecfedad258844acb2d7fb167d9125bd378ddf\
    0f02ae7589bd1a11e43ad71d2b3fcaaa28de6201d5b35d7a312b0c6c40035bb1\
    72fb294e5c6889087d4c1213e789ff7f0fe2a368362bc320e6e3cbe87be1cbe9\
    94380a9ba4a9eceb6db0b32c94673ce5ac7957e0855a5943398c092bb42d68e0";

const SIG_PSS_SHA384: &str = "\
    74671b5b2ce1ead0fcf0ed2246677acce18e060b7c379911bc95b5779f2e2046\
    31c9bc4fc0aa59118310a748d00bc9138846a7bbef61df4457a8761c52338aa8\
    eac3367a3e5834354dc75a9a617bdc0fd9cab10c7ac100e6fd75e8ea0c0c66e6\
    206ab0a64a7795d57a7b70d96508635f21d21f2353e5ff49f214c8eda812fd5a\
    3eab5fbbfb246a3ca959371392833a98a758407d0bb9b69b5c60327954f1dc46\
    319bd08d86a63e84b6cfa613a86cc22f4782e773467973e9d68d75ce162d714c\
    44e3a29c66ea5dddc1d8900b4c9ddfa7fcc98dc97c4e7ac601d0a263598da52d\
    2e3b58fc9a628af4845eeacc7085ef1cc42f47070c201e60cc5033d947925c8d";

const SIG_PSS_SHA512: &str = "\
    50f851038f2d347c47b130aa66d26b80447141db9151099e3396356523cb1a82\
    523891471d5da980e9a10e9e6150d57b2f4358f0b61a346d16592eb73b214304\
    5b256215915fc24226e6430f7effb8711842924b141649527bc2d07122fcdcbc\
    1631a648a14657f10b9ec66359ebdf1003acc49fd1a64c8febca0b3b6b7e0e52\
    e2c093c05bad52af3094131b264dd2af5f3b28d263ff6c493d2b3cb131369964\
    5b996a71972deb1e622f48491ef47d8dcb16433e45a91ec739327ece0430a8e4\
    fe6b275a1895509c1d3a5e683b75dabc4eb7bbcbb9b7a233ae4fc1cc35d8d3fc\
    88054ac1bd1f81c576bbccf36f60c74c2ff0e768b74e6f32d9f7d537350049f6";

const SIG_PKCS1_SHA256: &str = "\
    0427e7ce505fb0a54fa2fef2855a585f65d977929bf7dc87b82ab73be90bd45f\
    52e57c06811a82cabb495e1cba6c9f0cb506b05a02524046665c6daabf6bc29c\
    af555e822de1750d3e91a86aa223e8dafa1a3a7e2d711ab5b5012f68ca5f0ee1\
    e60264642a38f953e55d028abc85ec45f6c664ac39b7a52498e88815ab0f9be2\
    0b91df80357f281134b67ff3dd0962585fe744726a5b9973e1ef773d159e5ef4\
    dcae54432caf0971d758f06ea901c632a54bb9b3d3a50f744788e1bc6ad331ee\
    8f32d141386a5de41049a33d89c0b1e162d36922b516be429614afa18ab06887\
    b8f8fc60274ba931aa6796a55402532c98ffe02293e11ed792780bf6d444847d";

const SIG_PKCS1_SHA384: &str = "\
    6c331206b6e1ba9c3a92c976efb0e79c0175375f8852c370fe73b90974b04c32\
    dc06a5762dec793b752a4f8a20f8a48e66e48e26c48ce112441ac816d5a55d1f\
    746bfdc074bc99c89124c682ad6aab4ac7f32d2d2066387497a8645d18a3ba21\
    2719856cba9b0613147f36e41c62c677f525e35648c791c231811934a4ba5cc0\
    639203f490e73016223a2708aa50099e896ae1975ad7556a677bcb70cc803bc8\
    5cfbc5351973303c73813a4576b41add9deb6e5181d8b5fc4fb6f7fe59a9bf0d\
    f1a8eceb0107fd706e4037f496c0f3e82bfb7bd112f1d0c37cb439f62056d02a\
    3ce3f6b9492ccc7ab4121ea233810354620a16536fb79828bfd17d060a2a9f0c";

fn cert(pem: &str) -> Certificate {
    let ders = pem_certificates(pem);
    assert_eq!(ders.len(), 1);
    Certificate::parse(&ders[0]).unwrap()
}

fn der(pem: &str) -> Vec<u8> {
    pem_certificates(pem).remove(0)
}

fn rsa_leaf_key() -> vnet::crypto::rsa::RsaPublicKey {
    match cert(RSALEAF_PEM).public_key {
        PublicKey::Rsa(k) => k,
        other => panic!("expected an RSA key, got {other:?}"),
    }
}

#[test]
fn rsa_pss_signatures() {
    let key = rsa_leaf_key();
    assert_eq!(key.bits(), 2048);
    for (alg, sig) in [
        (HashAlg::Sha256, SIG_PSS_SHA256),
        (HashAlg::Sha384, SIG_PSS_SHA384),
        (HashAlg::Sha512, SIG_PSS_SHA512),
    ] {
        let sig = hex(sig);
        assert!(key.verify_pss(alg, MSG, &sig), "{alg:?}");
        assert!(!key.verify_pss(alg, b"another message", &sig), "{alg:?}");
        let mut bad = sig.clone();
        bad[100] ^= 4;
        assert!(!key.verify_pss(alg, MSG, &bad), "{alg:?}");
        assert!(!key.verify_pkcs1(alg, MSG, &sig), "{alg:?}");
    }
    // A PSS signature checked with the wrong hash.
    assert!(!key.verify_pss(HashAlg::Sha384, MSG, &hex(SIG_PSS_SHA256)));
}

#[test]
fn rsa_pkcs1_v1_5_signatures() {
    let key = rsa_leaf_key();
    let sig256 = hex(SIG_PKCS1_SHA256);
    let sig384 = hex(SIG_PKCS1_SHA384);
    assert!(key.verify_pkcs1(HashAlg::Sha256, MSG, &sig256));
    assert!(key.verify_pkcs1(HashAlg::Sha384, MSG, &sig384));
    assert!(!key.verify_pkcs1(HashAlg::Sha384, MSG, &sig256));
    assert!(!key.verify_pkcs1(HashAlg::Sha256, b"vidrio signature vectoR", &sig256));
    assert!(!key.verify_pkcs1(HashAlg::Sha256, MSG, &sig256[1..]));
    assert!(!key.verify_pss(HashAlg::Sha256, MSG, &sig256));
}

#[test]
fn bigint_bytes_round_trip() {
    let n = BigUint::from_be_bytes(&hex("00000102030405060708090a0b"));
    assert_eq!(n.bits(), 81);
    assert_eq!(
        to_hex(&n.to_be_bytes(12).unwrap()),
        "000102030405060708090a0b"
    );
    assert_eq!(n.to_be_bytes(10), None);
    assert!(BigUint::from_be_bytes(&[0, 0]).is_zero());
}

#[test]
fn certificate_fields() {
    let root = cert(ROOT_PEM);
    assert!(root.is_self_issued());
    assert_eq!(root.version, 3);
    assert_eq!(root.basic_constraints, Some((true, None)));
    assert_eq!(root.not_before, days_from_civil(2025, 1, 1) * 86400);
    assert_eq!(root.not_after, days_from_civil(2045, 1, 1) * 86400);
    assert!(matches!(root.public_key, PublicKey::Rsa(_)));

    let inter = cert(INTER_PEM);
    assert_eq!(inter.issuer, root.subject);
    assert_eq!(inter.basic_constraints, Some((true, Some(0))));
    assert!(matches!(inter.public_key, PublicKey::EcP256(_)));
    root.verify_issued(&inter).unwrap();

    let leaf = cert(LEAF_PEM);
    assert_eq!(leaf.issuer, inter.subject);
    assert_eq!(leaf.basic_constraints, Some((false, None)));
    assert_eq!(leaf.dns_names, ["example.test", "*.wild.example.test"]);
    assert_eq!(
        leaf.ip_addresses,
        ["127.0.0.1".parse::<std::net::IpAddr>().unwrap()]
    );
    assert_eq!(leaf.common_names, ["vidrio test leaf"]);
    inter.verify_issued(&leaf).unwrap();
    // An RSA key cannot check an ECDSA signature.
    assert_eq!(root.verify_issued(&leaf), Err(CertError::BadSignature));
}

#[test]
fn dates_are_unix_days() {
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(2000, 3, 1), 11017);
    assert_eq!(days_from_civil(1969, 12, 31), -1);
}

#[test]
fn name_matching() {
    assert!(dns_name_matches("example.test", "example.test"));
    assert!(dns_name_matches("Example.TEST.", "example.test"));
    assert!(dns_name_matches(
        "*.web-platform.test",
        "www.web-platform.test"
    ));
    assert!(dns_name_matches(
        "*.web-platform.test",
        "xn--lve-6lad.web-platform.test"
    ));
    assert!(!dns_name_matches(
        "*.web-platform.test",
        "web-platform.test"
    ));
    assert!(!dns_name_matches(
        "*.web-platform.test",
        "a.www.web-platform.test"
    ));
    assert!(!dns_name_matches("*.test", "example.test"));
    assert!(!dns_name_matches(
        "w*.web-platform.test",
        "www.web-platform.test"
    ));
    assert!(!dns_name_matches("example.test", "other.test"));
}

fn at(year: i64) -> i64 {
    days_from_civil(year, 1, 1) * 86400
}

fn store() -> TrustStore {
    let mut store = TrustStore::new();
    assert_eq!(store.add_pem(ROOT_PEM), 1);
    store
}

#[test]
fn chain_validates_to_the_root() {
    let store = store();
    let chain = vec![der(LEAF_PEM), der(INTER_PEM)];
    for host in [
        "example.test",
        "EXAMPLE.test.",
        "a.wild.example.test",
        "127.0.0.1",
    ] {
        assert_eq!(store.verify_at(&chain, host, at(2030)), Ok(()), "{host}");
    }
    // Intermediates may arrive in any order, with extra certificates.
    let shuffled = vec![
        der(LEAF_PEM),
        der(RSALEAF_PEM),
        der(INTER_PEM),
        der(ROOT_PEM),
    ];
    assert_eq!(store.verify_at(&shuffled, "example.test", at(2030)), Ok(()));
    // An RSA leaf signed directly by the RSA root, as on the WPT server.
    let wpt = vec![der(RSALEAF_PEM)];
    assert_eq!(store.verify_at(&wpt, "web-platform.test", at(2030)), Ok(()));
    assert_eq!(
        store.verify_at(&wpt, "www1.web-platform.test", at(2030)),
        Ok(())
    );
}

#[test]
fn chain_failures() {
    let store = store();
    let chain = vec![der(LEAF_PEM), der(INTER_PEM)];
    let mismatch = |h: &str| Err(CertError::NameMismatch(h.to_string()));
    assert_eq!(
        store.verify_at(&chain, "wild.example.test", at(2030)),
        mismatch("wild.example.test")
    );
    assert_eq!(
        store.verify_at(&chain, "a.b.wild.example.test", at(2030)),
        mismatch("a.b.wild.example.test")
    );
    assert_eq!(
        store.verify_at(&chain, "127.0.0.2", at(2030)),
        mismatch("127.0.0.2")
    );
    assert_eq!(
        store.verify_at(&chain, "example.test", at(2050)),
        Err(CertError::Expired)
    );
    assert_eq!(
        store.verify_at(&chain, "example.test", at(2020)),
        Err(CertError::NotYetValid)
    );
    assert_eq!(
        store.verify_at(&[der(LEAF_PEM)], "example.test", at(2030)),
        Err(CertError::UnknownIssuer)
    );
    assert_eq!(
        TrustStore::new().verify_at(&chain, "example.test", at(2030)),
        Err(CertError::UnknownIssuer)
    );
    assert_eq!(
        store.verify_at(&[], "example.test", at(2030)),
        Err(CertError::Empty)
    );
    // A tampered signature.
    let mut tampered = der(LEAF_PEM);
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    assert_eq!(
        store.verify_at(&[tampered, der(INTER_PEM)], "example.test", at(2030)),
        Err(CertError::BadSignature)
    );
    // Issued by a certificate that is not a CA.
    assert_eq!(
        store.verify_at(
            &[der(BAD_PEM), der(RSALEAF_PEM)],
            "bad.example.test",
            at(2030)
        ),
        Err(CertError::NotCa)
    );
    assert_eq!(
        Certificate::parse(b"\x30\x03\x02\x01\x01").err(),
        Some(CertError::Malformed)
    );
}

#[test]
fn trusted_leaf_needs_no_issuer() {
    let mut store = TrustStore::new();
    store.add_pem(RSALEAF_PEM);
    assert_eq!(
        store.verify_at(&[der(RSALEAF_PEM)], "web-platform.test", at(2030)),
        Ok(())
    );
}

#[test]
fn pem_bundles_and_files() {
    let bundle = format!("junk\n{ROOT_PEM}\n# comment\n{INTER_PEM}\n");
    assert_eq!(pem_certificates(&bundle).len(), 2);
    let mut store = TrustStore::new();
    assert_eq!(store.add_pem(&bundle), 2);
    assert_eq!(store.add_pem(&bundle), 2);
    assert_eq!(store.len(), 2);

    let dir = std::env::temp_dir().join(format!("vnet-tls-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ca.pem");
    std::fs::write(&path, ROOT_PEM).unwrap();
    let mut store = TrustStore::new();
    assert_eq!(store.add_file(&path).unwrap(), 1);
    assert!(
        TrustStore::new()
            .add_file(&dir.join("missing.pem"))
            .is_err()
    );
    // The connector used for `--ca-cert` loads the same file.
    assert!(vnet::tls::Connector::with_roots(Some(path.as_path())).is_ok());
    let mut client = vnet::http::Client::new(vnet::resolve::Resolver::new(None));
    client.enable_https(Some(path.as_path())).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------- RFC 8448 §3
//
// Simple 1-RTT handshake: x25519, TLS_AES_128_GCM_SHA256, an RSA 1024 server
// certificate and an rsa_pss_rsae_sha256 CertificateVerify.

const CLIENT_PRIVATE: &str = "49af42ba7f7994852d713ef2784bcbcaa7911de26adc5642cb634540e7ea5005";
const CLIENT_PUBLIC: &str = "99381de560e4bd43d23d8e435a7dbafeb3c06e51c13cae4d5413691e529aaf2c";
const SERVER_PUBLIC: &str = "c9828876112095fe66762bdbf7c672e156d6cc253b833df1dd69b1b04e751f0f";
const CLIENT_HELLO: &str = "\
    010000c00303cb34ecb1e78163ba1c38c6dacb196a6dffa21a8d9912ec18a2ef\
    6283024dece7000006130113031302010000910000000b000900000673657276\
    6572ff01000100000a00140012001d0017001800190100010101020103010400\
    230000003300260024001d002099381de560e4bd43d23d8e435a7dbafeb3c06e\
    51c13cae4d5413691e529aaf2c002b0003020304000d0020001e040305030603\
    020308040805080604010501060102010402050206020202002d00020101001c\
    00024001";
const CLIENT_HELLO_RECORD: &str = "\
    16030100c4010000c00303cb34ecb1e78163ba1c38c6dacb196a6dffa21a8d99\
    12ec18a2ef6283024dece7000006130113031302010000910000000b00090000\
    06736572766572ff01000100000a00140012001d001700180019010001010102\
    0103010400230000003300260024001d002099381de560e4bd43d23d8e435a7d\
    bafeb3c06e51c13cae4d5413691e529aaf2c002b0003020304000d0020001e04\
    0305030603020308040805080604010501060102010402050206020202002d00\
    020101001c00024001";
const EARLY_SECRET: &str = "33ad0a1c607ec03b09e6cd9893680ce210adf300aa1f2660e1b22e10f170f92a";
const SHARED_SECRET: &str = "8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d";
const HANDSHAKE_SECRET: &str = "1dc826e93606aa6fdc0aadc12f741b01046aa6b99f691ed221a9f0ca043fbeac";
const HELLO_HASH: &str = "860c06edc07858ee8e78f0e7428c58edd6b43f2ca3e6e95f02ed063cf0e1cad8";
const CLIENT_HS_SECRET: &str = "b3eddb126e067f35a780b3abf45e2d8f3b1a950738f52e9600746a0e27a55a21";
const SERVER_HS_SECRET: &str = "b67b7d690cc16c4e75e54213cb2d37b4e9c912bcded9105d42befd59d391ad38";
const MASTER_SECRET: &str = "18df06843d13a08bf2a449844c5f8a478001bc4d4c627984d5a41da8d0402919";
const SERVER_HS_KEY: &str = "3fce516009c21727d0f2e4e86ee403bc";
const SERVER_HS_IV: &str = "5d313eb2671276ee13000b30";
const SERVER_HELLO_RECORD: &str = "\
    160303005a020000560303a6af06a4121860dc5e6e60249cd34c95930c8ac5cb\
    1434dac155772ed3e2692800130100002e00330024001d0020c9828876112095\
    fe66762bdbf7c672e156d6cc253b833df1dd69b1b04e751f0f002b00020304";
const SERVER_FLIGHT: &str = "\
    080000240022000a00140012001d00170018001901000101010201030104001c\
    00024001000000000b0001b9000001b50001b0308201ac30820115a003020102\
    020102300d06092a864886f70d01010b0500300e310c300a0603550403130372\
    7361301e170d3136303733303031323335395a170d3236303733303031323335\
    395a300e310c300a0603550403130372736130819f300d06092a864886f70d01\
    0101050003818d0030818902818100b4bb498f8279303d980836399b36c6988c\
    0c68de55e1bdb826d3901a2461eafd2de49a91d015abbc9a95137ace6c1af19e\
    aa6af98c7ced43120998e187a80ee0ccb0524b1b018c3e0b63264d449a6d38e2\
    2a5fda430846748030530ef0461c8ca9d9efbfae8ea6d1d03e2bd193eff0ab9a\
    8002c47428a6d35a8d88d79f7f1e3f0203010001a31a301830090603551d1304\
    023000300b0603551d0f0404030205a0300d06092a864886f70d01010b050003\
    81810085aad2a0e5b9276b908c65f73a7267170618a54c5f8a7b337d2df7a594\
    365417f2eae8f8a58c8f8172f9319cf36b7fd6c55b80f21a03015156726096fd\
    335e5e67f2dbf102702e608ccae6bec1fc63a42a99be5c3eb7107c3c54e9b9eb\
    2bd5203b1c3b84e0a8b2f759409ba3eac9d91d402dcc0cc8f8961229ac9187b4\
    2b4de100000f000084080400805a747c5d88fa9bd2e55ab085a61015b7211f82\
    4cd484145ab3ff52f1fda8477b0b7abc90db78e2d33a5c141a078653fa6bef78\
    0c5ea248eeaaa785c4f394cab6d30bbe8d4859ee511f602957b15411ac027671\
    459e46445c9ea58c181e818e95b8c3fb0bf3278409d3be152a3da5043e063dda\
    65cdf5aea20d53dfacd42f74f3140000209b9b141d906337fbd2cbdce71df4de\
    da4ab42c309572cb7fffee5454b78f0718";
const SERVER_FLIGHT_RECORD: &str = "\
    17030302a2d1ff334a56f5bff6594a07cc87b580233f500f45e489e7f33af35e\
    df7869fcf40aa40aa2b8ea73f848a7ca07612ef9f945cb960b4068905123ea78\
    b111b429ba9191cd05d2a389280f526134aadc7fc78c4b729df828b5ecf7b13b\
    d9aefb0e57f271585b8ea9bb355c7c79020716cfb9b1183ef3ab20e37d57a6b9\
    d7477609aee6e122a4cf51427325250c7d0e509289444c9b3a648f1d71035d2e\
    d65b0e3cdd0cbae8bf2d0b227812cbb360987255cc744110c453baa4fcd61092\
    8d809810e4b7ed1a8fd991f06aa6248204797e36a6a73b70a2559c09ead68694\
    5ba246ab66e5edd8044b4c6de3fcf2a89441ac66272fd8fb330ef8190579b368\
    4596c960bd596eea520a56a8d650f563aad27409960dca63d3e688611ea5e22f\
    4415cf9538d51a200c27034272968a264ed6540c84838d89f72c24461aad6d26\
    f59ecaba9acbbb317b66d902f4f292a36ac1b639c637ce343117b65962224531\
    7b49eeda0c6258f100d7d961ffb138647e92ea330faeea6dfa31c7a84dc3bd7e\
    1b7a6c7178af36879018e3f252107f243d243dc7339d5684c8b0378bf30244da\
    8c87c843f5e56eb4c5e8280a2b48052cf93b16499a66db7cca71e4599426f7d4\
    61e66f99882bd89fc50800becca62d6c74116dbd2972fda1fa80f85df881edbe\
    5a37668936b335583b599186dc5c6918a396fa48a181d6b6fa4f9d62d513afbb\
    992f2b992f67f8afe67f76913fa388cb5630c8ca01e0c65d11c66a1e2ac4c859\
    77b7c7a6999bbf10dc35ae69f5515614636c0b9b68c19ed2e31c0b3b66763038\
    ebba42f3b38edc0399f3a9f23faa63978c317fc9fa66a73f60f0504de93b5b84\
    5e275592c12335ee340bbc4fddd502784016e4b3be7ef04dda49f4b440a30cb5\
    d2af939828fd4ae3794e44f94df5a631ede42c1719bfdabf0253fe5175be898e\
    750edc53370d2b";
const SERVER_FINISHED_KEY: &str =
    "008d3b66f816ea559f96b537e885c31fc068bf492c652f01f288a1d8cdc19fc8";
const SERVER_FINISHED: &str = "9b9b141d906337fbd2cbdce71df4deda4ab42c309572cb7fffee5454b78f0718";
const SERVER_FINISHED_HASH_INPUT: &str =
    "9608102a0f1ccc6db6250b7b7e417b1a000eaada3daae4777a7686c9ff83df13";
const CLIENT_AP_SECRET: &str = "9e40646ce79a7f9dc05af8889bce6552875afa0b06df0087f792ebb7c17504a5";
const SERVER_AP_SECRET: &str = "a11af9f05531f856ad47116b45a950328204b4f44bfb6b3a4b4f1f3fcb631643";
const CLIENT_FINISHED_RECORD: &str = "\
    170303003575ec4dc238cce60b298044a71e219c56cc77b0517fe9b93c7a4bfc\
    44d87f38f80338ac98fc46deb384bd1caeacab6867d726c40546";
const TICKET_RECORD: &str = "\
    17030300de3a6b8f90414a97d6959c3487680de5134a2b240e6cffac116e95d4\
    1d6af8f6b580dcf3d11d63c758db289a015940252f55713e061dc13e078891a3\
    8efbcf5753ad8ef170ad3c7353d16d9da773b9ca7f2b9fa1b6c0d4a3d03f75e0\
    9c30ba1e62972ac46f75f7b981be63439b2999ce13064615139891d5e4c5b406\
    f16e3fc181a77ca475840025db2f0a77f81b5ab05b94c01346755f69232c8651\
    9d86cbeeac87aac347d143f9605d64f650db4d023e70e952ca49fe5137121c74\
    bc2697687e248746d6df353005f3bce18696129c8153556b3b6c6779b37bf159\
    85684f";
const APP_PAYLOAD: &str = "\
    000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
    202122232425262728292a2b2c2d2e2f3031";
const CLIENT_APP_RECORD: &str = "\
    1703030043a23f7054b62c94d0affafe8228ba55cbefacea42f914aa66bcab3f\
    2b9819a8a5b46b395bd54a9a20441e2b62974e1f5a6292a2977014bd1e3deae6\
    3aeebb21694915e4";
const SERVER_APP_RECORD: &str = "\
    17030300432e937e11ef4ac740e538ad36005fc4a46932fc3225d05f82aa1b36\
    e30efaf97d90e6dffc602dcb501a59a8fcc49c4bf2e5f0a21c0047c2abf33254\
    0dd032e167c2955d";
const CLIENT_ALERT_RECORD: &str = "1703030013c9872760655666b74d7ff1153efd6db6d0b0e3";
const SERVER_ALERT_RECORD: &str = "1703030013b58fd67166ebf599d24720cfbe7efa7a8864a9";

#[test]
fn rfc_8448_key_schedule() {
    let alg = HashAlg::Sha256;
    let client = KeyShare::x25519(arr32(CLIENT_PRIVATE));
    assert_eq!(client.public(), &hex(CLIENT_PUBLIC)[..]);
    assert_eq!(client.agree(&hex(SERVER_PUBLIC)), Some(hex(SHARED_SECRET)));

    // Transcript hash of ClientHello and ServerHello.
    let mut transcript = hex(CLIENT_HELLO);
    transcript.extend_from_slice(&hex(SERVER_HELLO_RECORD)[5..]);
    assert_eq!(alg.digest(&transcript), hex(HELLO_HASH));

    let early = early_secret(alg);
    assert_eq!(early, hex(EARLY_SECRET));
    let hs = handshake_secret(alg, &early, &hex(SHARED_SECRET));
    assert_eq!(hs, hex(HANDSHAKE_SECRET));
    let c_hs = derive_secret(alg, &hs, b"c hs traffic", &hex(HELLO_HASH));
    let s_hs = derive_secret(alg, &hs, b"s hs traffic", &hex(HELLO_HASH));
    assert_eq!(c_hs, hex(CLIENT_HS_SECRET));
    assert_eq!(s_hs, hex(SERVER_HS_SECRET));
    let (key, iv) = traffic_keys(CipherSuite::Aes128GcmSha256, &s_hs);
    assert_eq!(key, hex(SERVER_HS_KEY));
    assert_eq!(iv.to_vec(), hex(SERVER_HS_IV));
    assert_eq!(
        hkdf_expand_label(alg, &s_hs, b"finished", &[], 32),
        hex(SERVER_FINISHED_KEY)
    );
    let master = master_secret(alg, &hs);
    assert_eq!(master, hex(MASTER_SECRET));
    let th = hex(SERVER_FINISHED_HASH_INPUT);
    assert_eq!(
        derive_secret(alg, &master, b"c ap traffic", &th),
        hex(CLIENT_AP_SECRET)
    );
    assert_eq!(
        derive_secret(alg, &master, b"s ap traffic", &th),
        hex(SERVER_AP_SECRET)
    );
}

#[test]
fn rfc_8448_record_protection() {
    // The server's encrypted flight opens with the server handshake keys
    // and carries EncryptedExtensions through Finished.
    let record = hex(SERVER_FLIGHT_RECORD);
    let header: [u8; 5] = record[..5].try_into().unwrap();
    let mut opener = RecordCipher::new(CipherSuite::Aes128GcmSha256, &hex(SERVER_HS_SECRET));
    let (kind, content) = opener.open(&header, &record[5..]).unwrap();
    assert_eq!(kind, 22);
    assert_eq!(content, hex(SERVER_FLIGHT));
    let flight = hex(SERVER_FLIGHT);
    assert_eq!(&flight[flight.len() - 32..], &hex(SERVER_FINISHED)[..]);
    // Sealing the same content with a fresh cipher gives the same record.
    let mut sealer = RecordCipher::new(CipherSuite::Aes128GcmSha256, &hex(SERVER_HS_SECRET));
    assert_eq!(sealer.seal(22, &flight), record);
}

/// Accepts exactly the RFC 8448 server certificate for the name "server".
struct TraceVerifier(Vec<u8>);

impl ServerVerifier for TraceVerifier {
    fn verify(&self, chain: &[Vec<u8>], server_name: &str) -> Result<(), CertError> {
        assert_eq!(server_name, "server");
        if chain.len() == 1 && chain[0] == self.0 {
            Ok(())
        } else {
            Err(CertError::UnknownIssuer)
        }
    }
}

/// The server certificate inside the server's flight: EncryptedExtensions is
/// 40 bytes, then Certificate (4), context (1), list length (3), entry length (3).
fn trace_certificate() -> Vec<u8> {
    let flight = hex(SERVER_FLIGHT);
    assert_eq!(flight[40], 11);
    let len = ((flight[48] as usize) << 16) | ((flight[49] as usize) << 8) | flight[50] as usize;
    flight[51..51 + len].to_vec()
}

fn trace_connection(verifier: Arc<dyn ServerVerifier>) -> ClientConnection {
    let config = Arc::new(ClientConfig::new(verifier));
    let mut conn = ClientConnection::from_client_hello(
        config,
        "server",
        &hex(CLIENT_HELLO),
        vec![KeyShare::x25519(arr32(CLIENT_PRIVATE))],
    )
    .unwrap();
    assert_eq!(conn.take_outgoing(), hex(CLIENT_HELLO_RECORD));
    conn
}

fn trace_handshake() -> ClientConnection {
    let mut conn = trace_connection(Arc::new(TraceVerifier(trace_certificate())));
    conn.feed(&hex(SERVER_HELLO_RECORD)).unwrap();
    assert!(conn.is_handshaking());
    assert!(conn.take_outgoing().is_empty());
    conn.feed(&hex(SERVER_FLIGHT_RECORD)).unwrap();
    assert!(conn.is_connected());
    conn
}

#[test]
fn rfc_8448_full_handshake() {
    let mut conn = trace_handshake();
    assert_eq!(conn.cipher_suite(), Some(CipherSuite::Aes128GcmSha256));
    assert_eq!(conn.alpn_protocol(), None);
    assert_eq!(conn.take_outgoing(), hex(CLIENT_FINISHED_RECORD));

    // NewSessionTicket is ignored.
    conn.feed(&hex(TICKET_RECORD)).unwrap();
    assert!(conn.take_outgoing().is_empty());

    conn.write_plaintext(&hex(APP_PAYLOAD)).unwrap();
    assert_eq!(conn.take_outgoing(), hex(CLIENT_APP_RECORD));

    conn.feed(&hex(SERVER_APP_RECORD)).unwrap();
    let mut buf = [0u8; 128];
    let n = conn.read_plaintext(&mut buf);
    assert_eq!(&buf[..n], &hex(APP_PAYLOAD)[..]);
    assert_eq!(conn.read_plaintext(&mut buf), 0);

    assert!(!conn.peer_closed());
    conn.feed(&hex(SERVER_ALERT_RECORD)).unwrap();
    assert!(conn.peer_closed());
    conn.send_close_notify();
    assert_eq!(conn.take_outgoing(), hex(CLIENT_ALERT_RECORD));
}

#[test]
fn rfc_8448_handshake_fed_byte_by_byte() {
    let mut conn = trace_connection(Arc::new(TraceVerifier(trace_certificate())));
    let mut server = hex(SERVER_HELLO_RECORD);
    server.extend(hex(SERVER_FLIGHT_RECORD));
    server.extend(hex(TICKET_RECORD));
    server.extend(hex(SERVER_APP_RECORD));
    for b in server {
        conn.feed(&[b]).unwrap();
    }
    assert!(conn.is_connected());
    assert_eq!(conn.take_outgoing(), hex(CLIENT_FINISHED_RECORD));
    let mut buf = [0u8; 128];
    let n = conn.read_plaintext(&mut buf);
    assert_eq!(&buf[..n], &hex(APP_PAYLOAD)[..]);
}

#[test]
fn rfc_8448_rejected_certificate_sends_an_alert() {
    let mut conn = trace_connection(Arc::new(TraceVerifier(Vec::new())));
    conn.feed(&hex(SERVER_HELLO_RECORD)).unwrap();
    let err = conn.feed(&hex(SERVER_FLIGHT_RECORD)).unwrap_err();
    assert_eq!(err, TlsError::Certificate(CertError::UnknownIssuer));
    assert_eq!(err.alert(), alert::UNKNOWN_CA);
    // The alert goes out under the client handshake key.
    let out = conn.take_outgoing();
    let header: [u8; 5] = out[..5].try_into().unwrap();
    let mut opener = RecordCipher::new(CipherSuite::Aes128GcmSha256, &hex(CLIENT_HS_SECRET));
    assert_eq!(
        opener.open(&header, &out[5..]),
        Some((21, vec![2, alert::UNKNOWN_CA]))
    );
    // The connection stays failed.
    assert_eq!(conn.feed(&[]), Err(err));
    assert!(conn.write_plaintext(b"x").is_err());
}

#[test]
fn rfc_8448_corrupted_record_fails() {
    let mut conn = trace_connection(Arc::new(TraceVerifier(trace_certificate())));
    conn.feed(&hex(SERVER_HELLO_RECORD)).unwrap();
    let mut flight = hex(SERVER_FLIGHT_RECORD);
    flight[100] ^= 0x40;
    let err = conn.feed(&flight).unwrap_err();
    assert_eq!(err.alert(), alert::BAD_RECORD_MAC);
    assert!(!conn.take_outgoing().is_empty());
}

#[test]
fn rfc_8448_application_data_before_finished_is_refused() {
    let mut conn = trace_connection(Arc::new(TraceVerifier(trace_certificate())));
    conn.feed(&hex(SERVER_HELLO_RECORD)).unwrap();
    // A record under the server handshake key that claims to be application data.
    let mut sealer = RecordCipher::new(CipherSuite::Aes128GcmSha256, &hex(SERVER_HS_SECRET));
    let err = conn.feed(&sealer.seal(23, b"too early")).unwrap_err();
    assert_eq!(err.alert(), alert::UNEXPECTED_MESSAGE);
}

#[test]
fn rfc_8448_key_update() {
    let suite = CipherSuite::Aes128GcmSha256;
    let mut conn = trace_handshake();
    conn.take_outgoing();
    conn.feed(&hex(TICKET_RECORD)).unwrap();
    conn.feed(&hex(SERVER_APP_RECORD)).unwrap();
    let mut buf = [0u8; 128];
    conn.read_plaintext(&mut buf);

    // The server's third record under its application key: KeyUpdate with
    // update_requested.
    let s_ap = hex(SERVER_AP_SECRET);
    let mut server = RecordCipher::new(suite, &s_ap);
    server.seal(22, b"ticket");
    server.seal(23, b"data");
    conn.feed(&server.seal(22, &[24, 0, 0, 1, 1])).unwrap();

    // The client answers with KeyUpdate(update_not_requested) under its old key.
    let out = conn.take_outgoing();
    let mut client = RecordCipher::new(suite, &hex(CLIENT_AP_SECRET));
    let header: [u8; 5] = out[..5].try_into().unwrap();
    assert_eq!(
        client.open(&header, &out[5..]),
        Some((22, vec![24, 0, 0, 1, 0]))
    );

    // Both directions now use the next secrets.
    let mut server = RecordCipher::new(suite, &next_traffic_secret(HashAlg::Sha256, &s_ap));
    conn.feed(&server.seal(23, b"after update")).unwrap();
    let n = conn.read_plaintext(&mut buf);
    assert_eq!(&buf[..n], b"after update");
    conn.write_plaintext(b"ping").unwrap();
    let out = conn.take_outgoing();
    let c_ap1 = next_traffic_secret(HashAlg::Sha256, &hex(CLIENT_AP_SECRET));
    let mut client = RecordCipher::new(suite, &c_ap1);
    let header: [u8; 5] = out[..5].try_into().unwrap();
    assert_eq!(
        client.open(&header, &out[5..]),
        Some((23, b"ping".to_vec()))
    );

    // A KeyUpdate sent by the client moves its own key forward.
    conn.send_key_update(false).unwrap();
    let out = conn.take_outgoing();
    let header: [u8; 5] = out[..5].try_into().unwrap();
    assert_eq!(
        client.open(&header, &out[5..]),
        Some((22, vec![24, 0, 0, 1, 0]))
    );
    conn.write_plaintext(b"pong").unwrap();
    let out = conn.take_outgoing();
    let mut client = RecordCipher::new(suite, &next_traffic_secret(HashAlg::Sha256, &c_ap1));
    let header: [u8; 5] = out[..5].try_into().unwrap();
    assert_eq!(
        client.open(&header, &out[5..]),
        Some((23, b"pong".to_vec()))
    );
}

// ---------------------------------------------------------------- HelloRetryRequest

struct RejectAll;

impl ServerVerifier for RejectAll {
    fn verify(&self, _: &[Vec<u8>], _: &str) -> Result<(), CertError> {
        Err(CertError::UnknownIssuer)
    }
}

/// Records split from a byte stream.
fn records(mut data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while !data.is_empty() {
        let len = u16::from_be_bytes([data[3], data[4]]) as usize;
        out.push(data[..5 + len].to_vec());
        data = &data[5 + len..];
    }
    out
}

/// legacy_session_id of the ClientHello in `record`.
fn session_id(record: &[u8]) -> Vec<u8> {
    let at = 5 + 4 + 2 + 32;
    record[at + 1..at + 1 + record[at] as usize].to_vec()
}

fn server_hello(random: &[u8; 32], session_id: &[u8], extensions: &[u8]) -> Vec<u8> {
    let mut body = vec![3, 3];
    body.extend_from_slice(random);
    body.push(session_id.len() as u8);
    body.extend_from_slice(session_id);
    body.extend([0x13, 0x01, 0]);
    body.extend((extensions.len() as u16).to_be_bytes());
    body.extend_from_slice(extensions);
    let mut msg = vec![2, 0, (body.len() >> 8) as u8, body.len() as u8];
    msg.extend(body);
    let mut record = vec![22, 3, 3, (msg.len() >> 8) as u8, msg.len() as u8];
    record.extend(msg);
    record
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn hello_retry_request_switches_to_secp256r1() {
    let config = Arc::new(ClientConfig::new(Arc::new(RejectAll)));
    let mut conn = ClientConnection::new(config, "example.test").unwrap();
    let first = conn.take_outgoing();
    assert_eq!(&first[..3], &[22, 3, 1]);
    // SNI, ALPN http/1.1 and an x25519 share in the first ClientHello.
    assert!(contains(
        &first,
        b"\x00\x00\x00\x11\x00\x0f\x00\x00\x0cexample.test"
    ));
    assert!(contains(&first, b"\x00\x10\x00\x0b\x00\x09\x08http/1.1"));
    assert!(contains(
        &first,
        &[0x00, 0x33, 0x00, 0x26, 0x00, 0x24, 0x00, 0x1d, 0x00, 0x20]
    ));
    let sid = session_id(&first);
    assert_eq!(sid.len(), 32);

    let hrr_random: [u8; 32] =
        hex("cf21ad74e59a6111be1d8c021e65b891c2a211167abb8c5e079e09e2c8a8339c")
            .try_into()
            .unwrap();
    let exts = [
        0x00, 0x2b, 0x00, 0x02, 0x03, 0x04, // supported_versions: TLS 1.3
        0x00, 0x33, 0x00, 0x02, 0x00, 0x17, // key_share: secp256r1
        0x00, 0x2c, 0x00, 0x09, 0x00, 0x07, b'c', b'o', b'o', b'k', b'i', b'e', b'!',
    ];
    conn.feed(&server_hello(&hrr_random, &sid, &exts)).unwrap();
    assert!(conn.is_handshaking());
    let out = records(&conn.take_outgoing());
    assert_eq!(out.len(), 2);
    // Middlebox compatibility: a ChangeCipherSpec before the second flight.
    assert_eq!(out[0], [20, 3, 3, 0, 1, 1]);
    let second = &out[1];
    assert_eq!(&second[..3], &[22, 3, 3]);
    assert_eq!(&second[11..43], &first[11..43], "same random");
    assert_eq!(session_id(second), sid);
    assert!(contains(
        second,
        &[
            0x00, 0x33, 0x00, 0x47, 0x00, 0x45, 0x00, 0x17, 0x00, 0x41, 0x04
        ]
    ));
    assert!(contains(second, b"\x00\x2c\x00\x09\x00\x07cookie!"));

    // A second HelloRetryRequest is a protocol error.
    let err = conn
        .feed(&server_hello(&hrr_random, &sid, &exts))
        .unwrap_err();
    assert_eq!(err.alert(), alert::UNEXPECTED_MESSAGE);
}

#[test]
fn hello_retry_request_for_an_offered_share_is_illegal() {
    let config = Arc::new(ClientConfig::new(Arc::new(RejectAll)));
    let mut conn = ClientConnection::new(config, "example.test").unwrap();
    let sid = session_id(&conn.take_outgoing());
    let hrr_random: [u8; 32] =
        hex("cf21ad74e59a6111be1d8c021e65b891c2a211167abb8c5e079e09e2c8a8339c")
            .try_into()
            .unwrap();
    let exts = [
        0x00, 0x2b, 0x00, 0x02, 0x03, 0x04, 0x00, 0x33, 0x00, 0x02, 0x00, 0x1d,
    ];
    let err = conn
        .feed(&server_hello(&hrr_random, &sid, &exts))
        .unwrap_err();
    assert_eq!(err.alert(), alert::ILLEGAL_PARAMETER);
    // The alert goes out in the clear before any key exists.
    assert_eq!(
        conn.take_outgoing(),
        [21, 3, 3, 0, 2, 2, alert::ILLEGAL_PARAMETER]
    );
}

#[test]
fn tls_1_2_server_hello_is_refused() {
    let config = Arc::new(ClientConfig::new(Arc::new(RejectAll)));
    let mut conn = ClientConnection::new(config, "127.0.0.1").unwrap();
    let first = conn.take_outgoing();
    // No SNI for an IP address.
    assert!(!contains(&first, b"127.0.0.1"));
    let sid = session_id(&first);
    let err = conn.feed(&server_hello(&[1; 32], &sid, &[])).unwrap_err();
    assert_eq!(err.alert(), alert::PROTOCOL_VERSION);
}

#[test]
fn server_hello_must_echo_the_session_id() {
    let config = Arc::new(ClientConfig::new(Arc::new(RejectAll)));
    let mut conn = ClientConnection::new(config, "example.test").unwrap();
    conn.take_outgoing();
    let exts = [0x00, 0x2b, 0x00, 0x02, 0x03, 0x04];
    let err = conn
        .feed(&server_hello(&[1; 32], &[0; 32], &exts))
        .unwrap_err();
    assert_eq!(err.alert(), alert::ILLEGAL_PARAMETER);
}

#[test]
fn random_bytes_differ() {
    let a: [u8; 32] = random::bytes().unwrap();
    let b: [u8; 32] = random::bytes().unwrap();
    assert_ne!(a, b);
}
