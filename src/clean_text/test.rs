use super::*;
use quickcheck::quickcheck;

lazy_static! {
    pub static ref RE_LIST: Vec<RegexCleanerPair> = crate::settings::Settings::new().cleaners;
}

pub fn clean_text_string<T: AsRef<str>>(raw: T, list: &[RegexCleanerPair]) -> String {
    clean_text(raw.as_ref(), list)
}

#[test]
fn one_word() {
    assert_eq!(clean_text_string("Hello", &RE_LIST), "Hello");
}

#[test]
fn in_string() {
    assert_eq!(clean_text_string("Hello".to_string(), &RE_LIST), "Hello");
}

#[test]
fn two_word() {
    assert_eq!(clean_text_string("Hello world!", &RE_LIST), "Hello world!");
}

#[test]
fn two_word_with_new_line() {
    assert_eq!(
        clean_text_string("Hello \t\n \t\r \t\r\n world!", &RE_LIST),
        "Hello world!"
    );
}

#[test]
fn two_word_with_tabs() {
    assert_eq!(
        clean_text_string("Hello\t\n\t\r\t\r\nworld!", &RE_LIST),
        "Hello world!"
    );
}

#[test]
fn sha1() {
    assert_eq!(
        clean_text_string(
            "1 parent 1b329f3 commit 4773d2e39d0be947344ddfebc92d16f37e0584aa",
            &RE_LIST,
        ),
        "1 parent 1b329f3 commit hash 4773d2"
    );
    assert_eq!(
        clean_text_string(
            "hash in code <4773d2e39d0be947344ddfebc92d16f37e0584aa>",
            &RE_LIST,
        ),
        "hash in code <hash 4773d2>"
    );
}

#[test]
fn url() {
    assert_eq!(
        clean_text_string("https://www.youtube.com/watch?v=JFpanWNgfQY", &RE_LIST),
        "link to www.youtube.com"
    );
    assert_eq!(
        clean_text_string("www.youtube.com/watch?v=JFpanWNgfQY", &RE_LIST),
        "link to www.youtube.com"
    );
    assert_eq!(
        clean_text_string(
            "link in code <www.youtube.com/watch?v=JFpanWNgfQY>",
            &RE_LIST,
        ),
        "link in code <link to www.youtube.com>"
    );
}

#[test]
fn two_word_with_underscore() {
    assert_eq!(
        clean_text_string("Hello _________ world!", &RE_LIST),
        "Hello ___ world!"
    );
}

#[test]
fn two_word_with_dash() {
    assert_eq!(
        clean_text_string("Hello ----------- world!", &RE_LIST),
        "Hello --- world!"
    );
}

#[test]
fn two_word_with_dash_u8idx() {
    let text = "Hello \u{5d4}\u{5a2}\u{5d4}\u{5a2}\
                \u{5d4}\u{5a2}\u{5d4}\u{5a2}\u{5d4}\u{5a2} ----------- \u{1d565}\
                \u{1d565}\u{1d565}\u{1d565}\u{1d565}       ";
    assert_eq!(
        clean_text_u8idx_in(text, &RE_LIST),
        vec![
            0, 1, 2, 3, 4, 5, 6, 10, 14, 18, 22, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37,
            38, 39, 43, 47, 51, 55, 59, 66,
        ]
    );
    assert_eq!(
        clean_text_u8idx_out(text, &RE_LIST),
        vec![
            0, 1, 2, 3, 4, 5, 6, 10, 14, 18, 18, 18, 19, 20, 21, 22, 22, 22, 22, 22, 22, 22, 22,
            22, 23, 27, 31, 35, 35, 35, 36,
        ]
    );
}

#[test]
fn two_word_with_dash_u16idx() {
    let text = "Hello \u{5d4}\u{5a2}\u{5d4}\u{5a2}\
                \u{5d4}\u{5a2}\u{5d4}\u{5a2}\u{5d4}\u{5a2} ----------- \u{1d565}\
                \u{1d565}\u{1d565}\u{1d565}\u{1d565}       ";
    assert_eq!(
        clean_text_u16idx_in(text, &RE_LIST),
        vec![
            0, 1, 2, 3, 4, 5, 6, 8, 10, 12, 14, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
            29, 31, 33, 35, 37, 39, 46,
        ]
    );
    assert_eq!(
        clean_text_u16idx_out(text, &RE_LIST),
        vec![
            0, 1, 2, 3, 4, 5, 6, 8, 10, 12, 12, 12, 13, 14, 15, 16, 16, 16, 16, 16, 16, 16, 16, 16,
            17, 19, 21, 23, 23, 23, 24,
        ]
    );
}

#[test]
fn two_word_with_equals() {
    assert_eq!(
        clean_text_string("Hello =========== world!", &RE_LIST),
        "Hello === world!"
    );
}

#[test]
fn two_word_with_numbers() {
    assert_eq!(
        clean_text_string("Hello 100000 world!", &RE_LIST),
        "Hello 100000 world!"
    );
}

#[test]
fn two_word_with_longchar() {
    assert_eq!(
        clean_text_string(
            "Hello \u{1d565}\u{1d565}\u{1d565}\u{1d565}\u{1d565} world!",
            &RE_LIST,
        ),
        "Hello \u{1d565}\u{1d565}\u{1d565} world!"
    );
}

#[test]
fn two_word_with_multichar() {
    assert_eq!(
        clean_text_string(
            "Hello \u{5d4}\u{5a2}\u{5d4}\u{5a2}\u{5d4}\u{5a2}\u{5d4}\u{5a2}\
             \u{5d4}\u{5a2} world!",
            &RE_LIST,
        ),
        "Hello \u{5d4}\u{5a2}\u{5d4}\u{5a2}\u{5d4}\u{5a2} world!"
    );
}

fn test_clean_text_u8idx<T: AsRef<str>>(text: T) -> bool {
    let text = text.as_ref();
    let vec_u8idx_in = clean_text_u8idx_in(text, &RE_LIST);
    let vec_u8idx_out = clean_text_u8idx_out(text, &RE_LIST);
    for (&in_idx, &out_idx) in vec_u8idx_in.iter().zip(vec_u8idx_out.iter()) {
        if clean_text_string(&text[..in_idx], &RE_LIST).len() != out_idx {
            println!("\r\n{:?}", vec_u8idx_in);
            println!("{:?}", vec_u8idx_out);
            println!(
                "({:?}, {:?}) {:?}",
                in_idx,
                out_idx,
                clean_text_string(&text[..in_idx], &RE_LIST).len()
            );
            return false;
        }
    }
    true
}

#[test]
fn tests_clean_text_u8idx() {
    assert!(test_clean_text_u8idx("Hello"));
    assert!(test_clean_text_u8idx("Hello\t\n\t\r\t\r\nworld!"));
    assert!(test_clean_text_u8idx("Hello _________ world!"));
    assert!(test_clean_text_u8idx("Hello 100000 world!"));
    assert!(test_clean_text_u8idx(
        "Hello \u{1d565}\u{1d565}\u{1d565}\u{1d565}\u{1d565} \
         world!",
    ));
    assert!(test_clean_text_u8idx(
        "Hello \u{5d4}\u{5a2}\u{5d4}\u{5a2}\u{5d4}\
         \u{5a2}\u{5d4}\u{5a2}\u{5d4}\u{5a2} world!",
    ));
}

#[test]
fn quickcheck_clean_text_u8idx() {
    quickcheck(test_clean_text_u8idx as fn(String) -> bool);
}

fn test_does_not_lose_segments<T: AsRef<str>>(text: T) -> bool {
    let text = text.as_ref();
    let left_out: String = clean_iter(text, &RE_LIST).map(|(o, _)| o).collect();
    text == left_out
}

#[test]
fn quickcheck_does_not_lose_segments() {
    quickcheck(test_does_not_lose_segments as fn(String) -> bool);
}
