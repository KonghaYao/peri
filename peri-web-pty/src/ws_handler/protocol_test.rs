use super::*;

#[test]
fn multibyte_output_survives_every_read_boundary() {
    let expected = "ASCII终端🙂\x1b[31m尾部";
    let mut decoder = OutputDecoder::default();
    let mut actual = String::new();
    for byte in expected.as_bytes() {
        let (text, reply) = decoder.push(&[*byte]);
        assert!(!reply);
        actual.push_str(&text);
    }
    actual.push_str(&decoder.finish());
    assert_eq!(actual, expected);
}

#[test]
fn dsr_split_across_reads_is_replied_to_once_without_removing_output() {
    let mut decoder = OutputDecoder::default();
    let mut actual = String::new();
    let mut replies = 0;
    for bytes in [b"start\x1b[".as_slice(), b"6", b"nnext\x1b[6n"] {
        let (text, reply) = decoder.push(bytes);
        actual.push_str(&text);
        replies += usize::from(reply);
    }
    assert_eq!(replies, 1);
    assert_eq!(actual, "start\x1b[6nnext\x1b[6n");
}

#[test]
fn incomplete_final_character_is_flushed_once() {
    let mut decoder = OutputDecoder::default();
    assert_eq!(decoder.push(b"tail\xf0\x9f").0, "tail");
    assert_eq!(decoder.finish(), "�");
    assert_eq!(decoder.finish(), "");
}
