use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn padded<const N: usize>(stored: &[u8]) -> Result<[u8; N], Box<dyn std::error::Error>> {
    let mut field = [0; N];
    field
        .get_mut(..stored.len())
        .ok_or("stored bytes longer than the field")?
        .copy_from_slice(stored);
    Ok(field)
}

#[test]
fn latin1_and_two_byte_characters_take_their_stored_forms() -> TestResult {
    for (text, stored) in [
        ("Akihabara", &b"Akihabara"[..]),
        ("Gm\u{fc}nd", b"Gm\xfcnd"),
        ("\u{15a}wi\u{119}tokrzyskie", b"\x01Zwi\x01\x19tokrzyskie"),
        ("Gmina Pas\u{142}\u{119}k", b"Gmina Pas\x01B\x01\x19k"),
        ("", b""),
    ] {
        let encoded = StoredText::<16>::new(text)?;
        assert_eq!(encoded.as_bytes(), &padded::<16>(stored)?, "{text}");
        let decoded = StoredText::<16>::from_bytes(padded(stored)?)?;
        assert_eq!(decoded.to_string(), text);
        assert_eq!(decoded, encoded);
    }
    assert!(StoredText::<16>::new("")?.is_empty());
    assert_eq!(StoredText::<16>::EMPTY.as_bytes(), &[0; 16]);
    Ok(())
}

#[test]
fn the_field_width_bounds_the_stored_form() -> TestResult {
    let memo = "Thirty-two characters of memo!!!";
    assert_eq!(StoredText::<32>::new(memo)?.to_string(), memo);
    assert!(StoredText::<32>::new("")?.is_empty() && StoredText::<0>::EMPTY.is_empty());
    let result = StoredText::<16>::new(memo);
    assert!(
        matches!(
            result,
            Err(StoredTextError::NotStorable { capacity: 16, .. })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn text_without_a_stored_form_is_refused() {
    for text in [
        "\u{100}",
        "\u{218}",
        "\u{6771}",
        "tab\there",
        "Seventeen bytes!!",
        "\u{15a}wi\u{119}tokrzyskie!",
    ] {
        let result = StoredText::<16>::new(text);
        assert!(
            matches!(result, Err(StoredTextError::NotStorable { .. })),
            "{text:?}: {result:?}"
        );
    }
}

#[test]
fn malformed_stored_text_is_refused() -> TestResult {
    let mut after_padding = padded::<16>(b"ab")?;
    *after_padding.get_mut(5).ok_or("byte 5")? = b'c';
    for raw in [
        padded::<16>(b"abc\x01")?,
        padded(b"abcdefghijklmno\x01")?,
        padded(b"tab\x09")?,
        padded(b"del\x7f")?,
        padded(b"c1\x85")?,
        after_padding,
    ] {
        let result = StoredText::<16>::from_bytes(raw);
        assert!(
            matches!(result, Err(StoredTextError::Malformed { .. })),
            "{raw:02X?}: {result:?}"
        );
    }
    Ok(())
}
