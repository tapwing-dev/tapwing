//! Host-side random input generation matching Maestro's `InputRandomCommand.genRandomString()`
//! (`maestro-orchestra-models/.../Commands.kt:795-810`) and backing native Rust
//! `faker` providers for the embedded JS engine.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::flow::ir::InputRandomType;

const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

const FIRST_NAMES: &[&str] = &[
    "Alex", "Sam", "Jordan", "Taylor", "Morgan", "Casey", "Riley", "Jamie", "Quinn", "Avery", "Elena", "Marcus",
    "Clara", "Liam", "Noah", "Maya", "Lucas", "Nora", "Owen", "Hannah",
];

const LAST_NAMES: &[&str] = &[
    "Miller", "Smith", "Johnson", "Brown", "Davis", "Wilson", "Clark", "Lewis", "Walker", "Hall", "Young", "Allen",
    "King", "Wright", "Scott", "Green", "Baker", "Adams", "Nelson", "Carter",
];

const JOB_TITLES: &[&str] = &[
    "Software Engineer",
    "Product Manager",
    "Designer",
    "Data Scientist",
    "QA Analyst",
    "Systems Architect",
    "Tech Lead",
    "Operations Manager",
];

const DOMAINS: &[&str] = &["example.com", "test.org", "mail.net", "sample.io"];

const CITIES: &[&str] = &[
    "London",
    "Tokyo",
    "Prague",
    "Seattle",
    "Austin",
    "Berlin",
    "Toronto",
    "Sydney",
    "Dublin",
    "Zurich",
    "Lisbon",
    "Vienna",
    "Denver",
    "Boston",
    "Osaka",
    "Montreal",
    "Stockholm",
    "Munich",
    "Chicago",
    "Portland",
];

const COUNTRIES: &[&str] = &[
    "Canada",
    "Japan",
    "Germany",
    "France",
    "Spain",
    "Italy",
    "Sweden",
    "Norway",
    "Brazil",
    "Australia",
    "Ireland",
    "Switzerland",
    "Portugal",
    "Austria",
    "Poland",
    "Czechia",
    "Finland",
    "Belgium",
    "Netherlands",
    "Mexico",
];

const COUNTRY_CODES: &[&str] = &[
    "CA", "JP", "DE", "FR", "ES", "IT", "SE", "NO", "BR", "AU", "IE", "CH", "PT", "AT", "PL", "CZ", "FI", "BE", "NL",
    "MX",
];

const STATES: &[&str] =
    &["California", "Texas", "New York", "Washington", "Colorado", "Oregon", "Illinois", "Massachusetts"];

const STREETS: &[&str] = &["Oak", "Maple", "Cedar", "Pine", "Elm", "Birch", "Willow", "Sunset", "Highland", "Park"];
const STREET_SUFFIXES: &[&str] = &["St", "Ave", "Blvd", "Rd", "Ln", "Dr", "Way"];

const COLORS: &[&str] = &[
    "red", "blue", "green", "yellow", "purple", "orange", "teal", "crimson", "indigo", "amber", "violet", "coral",
    "silver", "gold", "ivory", "navy", "olive", "cyan", "magenta", "maroon",
];

const LOREM_WORDS: &[&str] = &[
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
    "enim",
    "ad",
    "minim",
    "veniam",
    "quis",
    "nostrud",
    "exercitation",
    "ullamco",
    "laboris",
    "nisi",
    "aliquip",
];

const COMPANIES: &[&str] =
    &["Acme Corp", "Globex", "Initech", "Northwind", "Contoso", "Umbrella Labs", "Apex Systems", "Solstice Tech"];

const PRODUCTS: &[&str] =
    &["Wireless Keyboard", "Ergonomic Mouse", "Mechanical Watch", "Ceramic Mug", "Canvas Backpack", "Desk Lamp"];

static COUNTER: AtomicU64 = AtomicU64::new(1);

pub struct Rng(u64);

impl Rng {
    pub fn from_clock() -> Self {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0x9e37_79b9);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let seed = nanos ^ seq.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        Self(if seed == 0 { 0xdead_beef_cafe_babe } else { seed })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        items[(self.next_u64() as usize) % items.len()]
    }

    pub fn range(&mut self, max_exclusive: usize) -> usize {
        if max_exclusive == 0 { 0 } else { (self.next_u64() as usize) % max_exclusive }
    }
}

/// Generates a random string matching Maestro's `InputRandomCommand.genRandomString()`
/// (`Commands.kt:795-810`). Default length is 8; non-positive lengths fall back to 8.
pub fn generate(input_type: Option<InputRandomType>, length: Option<i32>) -> String {
    let mut rng = Rng::from_clock();
    generate_with_rng(input_type, length, &mut rng)
}

fn generate_with_rng(input_type: Option<InputRandomType>, length: Option<i32>, rng: &mut Rng) -> String {
    let len = match length {
        Some(n) if n > 0 => n as usize,
        _ => 8,
    };
    match input_type.unwrap_or(InputRandomType::Text) {
        InputRandomType::Number => random_number_str(len, rng),
        InputRandomType::Text => random_text_str(len, rng),
        InputRandomType::TextEmailAddress => email_with_rng(rng),
        InputRandomType::TextPersonName => format!("{} {}", rng.pick(FIRST_NAMES), rng.pick(LAST_NAMES)),
        InputRandomType::TextCityName => rng.pick(CITIES).to_string(),
        InputRandomType::TextCountryName => rng.pick(COUNTRIES).to_string(),
        InputRandomType::TextColor => rng.pick(COLORS).to_string(),
    }
}

fn random_number_str(len: usize, rng: &mut Rng) -> String {
    let len = len.max(1);
    let mut out = String::with_capacity(len);
    // First digit 1..=9 so the decimal representation always has exact length `len`
    // (matches `faker.number().randomNumber(finalLength).toString()` in CommandsTest.kt:117).
    out.push((b'1' + (rng.range(9) as u8)) as char);
    for _ in 1..len {
        out.push((b'0' + (rng.range(10) as u8)) as char);
    }
    out
}

fn random_text_str(len: usize, rng: &mut Rng) -> String {
    let len = len.max(1);
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        out.push(LETTERS[rng.range(LETTERS.len())] as char);
    }
    out
}

fn email_with_rng(rng: &mut Rng) -> String {
    let first = rng.pick(FIRST_NAMES).to_lowercase();
    let last = rng.pick(LAST_NAMES).to_lowercase();
    let num = 10 + rng.range(990);
    let domain = rng.pick(DOMAINS);
    format!("{first}.{last}{num}@{domain}")
}

// --- Native Rust faker helpers (exposed to QuickJS without any JS library overhead) ---

pub fn faker_first_name() -> String {
    Rng::from_clock().pick(FIRST_NAMES).to_string()
}

pub fn faker_last_name() -> String {
    Rng::from_clock().pick(LAST_NAMES).to_string()
}

pub fn faker_full_name() -> String {
    let mut rng = Rng::from_clock();
    format!("{} {}", rng.pick(FIRST_NAMES), rng.pick(LAST_NAMES))
}

pub fn faker_username() -> String {
    let mut rng = Rng::from_clock();
    format!("{}.{}{}", rng.pick(FIRST_NAMES).to_lowercase(), rng.pick(LAST_NAMES).to_lowercase(), rng.range(100))
}

pub fn faker_title() -> String {
    Rng::from_clock().pick(JOB_TITLES).to_string()
}

pub fn faker_email() -> String {
    email_with_rng(&mut Rng::from_clock())
}

pub fn faker_domain_name() -> String {
    Rng::from_clock().pick(DOMAINS).to_string()
}

pub fn faker_url() -> String {
    let mut rng = Rng::from_clock();
    format!("https://www.{}/{}", rng.pick(DOMAINS), rng.pick(LOREM_WORDS))
}

const MAX_FAKER_LEN: usize = 65_536;

pub fn faker_password(min_len: usize, max_len: usize) -> String {
    let mut rng = Rng::from_clock();
    let lo = min_len.clamp(1, MAX_FAKER_LEN);
    let hi = max_len.clamp(lo, MAX_FAKER_LEN);
    let len = lo + rng.range(hi - lo + 1);
    random_text_str(len, &mut rng)
}

pub fn faker_ipv4() -> String {
    let mut rng = Rng::from_clock();
    format!("{}.{}.{}.{}", 1 + rng.range(223), rng.range(256), rng.range(256), 1 + rng.range(254))
}

pub fn faker_uuid() -> String {
    let mut rng = Rng::from_clock();
    let a = rng.next_u64();
    let b = rng.next_u64();
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (a >> 32) as u32,
        ((a >> 16) & 0xffff) as u16,
        (a & 0x0fff) as u16,
        (((b >> 48) & 0x3fff) | 0x8000) as u16,
        b & 0xffff_ffff_ffff
    )
}

pub fn faker_city() -> String {
    Rng::from_clock().pick(CITIES).to_string()
}

pub fn faker_country() -> String {
    Rng::from_clock().pick(COUNTRIES).to_string()
}

pub fn faker_country_code() -> String {
    Rng::from_clock().pick(COUNTRY_CODES).to_string()
}

pub fn faker_state() -> String {
    Rng::from_clock().pick(STATES).to_string()
}

pub fn faker_street_name() -> String {
    let mut rng = Rng::from_clock();
    format!("{} {}", rng.pick(STREETS), rng.pick(STREET_SUFFIXES))
}

pub fn faker_street_address() -> String {
    let mut rng = Rng::from_clock();
    format!("{} {} {}", 10 + rng.range(9900), rng.pick(STREETS), rng.pick(STREET_SUFFIXES))
}

pub fn faker_zip_code() -> String {
    format!("{:05}", 10000 + Rng::from_clock().range(90000))
}

pub fn faker_full_address() -> String {
    let mut rng = Rng::from_clock();
    format!(
        "{} {} {}, {}, {} {:05}",
        10 + rng.range(9900),
        rng.pick(STREETS),
        rng.pick(STREET_SUFFIXES),
        rng.pick(CITIES),
        rng.pick(STATES),
        10000 + rng.range(90000)
    )
}

pub fn faker_color_name() -> String {
    Rng::from_clock().pick(COLORS).to_string()
}

pub fn faker_color_hex() -> String {
    format!("#{:06X}", Rng::from_clock().range(0x1000000))
}

pub fn faker_number_digits(count: usize) -> String {
    let mut rng = Rng::from_clock();
    let n = count.clamp(1, MAX_FAKER_LEN);
    (0..n).map(|_| (b'0' + rng.range(10) as u8) as char).collect()
}

pub fn faker_random_number(digits: usize) -> i64 {
    let mut rng = Rng::from_clock();
    random_number_str(digits.clamp(1, 18), &mut rng).parse::<i64>().unwrap_or(1)
}

pub fn faker_number_between(min: i64, max: i64) -> i64 {
    if max <= min {
        return min;
    }
    let span = max.abs_diff(min);
    min.wrapping_add_unsigned(Rng::from_clock().next_u64() % span)
}

pub fn faker_random_double(decimals: usize, min: i64, max: i64) -> f64 {
    let mut rng = Rng::from_clock();
    let lo = min.min(max) as f64;
    let hi = min.max(max) as f64;
    let frac = (rng.next_u64() as f64) / (u64::MAX as f64);
    let val = lo + frac * (hi - lo);
    let factor = 10_f64.powi(decimals.min(9) as i32);
    (val * factor).round() / factor
}

pub fn faker_text(len: usize) -> String {
    let clamped = if len == 0 { 8 } else { len.min(MAX_FAKER_LEN) };
    random_text_str(clamped, &mut Rng::from_clock())
}

pub fn faker_word() -> String {
    Rng::from_clock().pick(LOREM_WORDS).to_string()
}

pub fn faker_words(count: usize) -> String {
    let mut rng = Rng::from_clock();
    let n = count.max(1);
    (0..n).map(|_| rng.pick(LOREM_WORDS)).collect::<Vec<_>>().join(" ")
}

pub fn faker_sentence(words: usize) -> String {
    let mut s = faker_words(words.max(3));
    if let Some(first) = s.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    s.push('.');
    s
}

pub fn faker_phone_number() -> String {
    let mut rng = Rng::from_clock();
    format!("{:03}-{:03}-{:04}", 200 + rng.range(800), 200 + rng.range(800), rng.range(10000))
}

pub fn faker_company() -> String {
    Rng::from_clock().pick(COMPANIES).to_string()
}

pub fn faker_product_name() -> String {
    Rng::from_clock().pick(PRODUCTS).to_string()
}

pub fn faker_price() -> String {
    let mut rng = Rng::from_clock();
    format!("{}.{:02}", 1 + rng.range(999), rng.range(100))
}

pub fn faker_bool() -> bool {
    (Rng::from_clock().next_u64() & 1) == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_number_exact_length_and_digits() {
        let s = generate(Some(InputRandomType::Number), Some(10));
        assert_eq!(s.len(), 10);
        assert!(s.chars().all(|c| c.is_ascii_digit()));
        assert!(!s.starts_with('0'));

        let fallback = generate(Some(InputRandomType::Number), Some(0));
        assert_eq!(fallback.len(), 8);
        let neg = generate(Some(InputRandomType::Number), Some(-3));
        assert_eq!(neg.len(), 8);
    }

    #[test]
    fn random_text_exact_length() {
        let s = generate(Some(InputRandomType::Text), Some(20));
        assert_eq!(s.len(), 20);
        assert!(s.chars().all(|c| c.is_ascii_alphabetic()));

        let default_none = generate(None, None);
        assert_eq!(default_none.len(), 8);
    }

    #[test]
    fn random_semantic_types_have_expected_shapes() {
        let email = generate(Some(InputRandomType::TextEmailAddress), None);
        assert!(email.contains('@') && email.contains('.'), "invalid email: {email}");

        let person = generate(Some(InputRandomType::TextPersonName), None);
        assert_eq!(person.split(' ').count(), 2, "expected 'First Last', got: {person}");

        assert!(!generate(Some(InputRandomType::TextCityName), None).is_empty());
        assert!(!generate(Some(InputRandomType::TextCountryName), None).is_empty());
        assert!(!generate(Some(InputRandomType::TextColor), None).is_empty());
    }

    #[test]
    fn faker_bounds_prevent_overflow_and_runaway_allocations() {
        let v = faker_number_between(i64::MIN, i64::MAX);
        assert!((i64::MIN..i64::MAX).contains(&v));
        assert_eq!(faker_number_between(10, 5), 10);
        assert_eq!(faker_text(1_000_000).len(), MAX_FAKER_LEN);
        assert_eq!(faker_number_digits(1_000_000).len(), MAX_FAKER_LEN);
        assert!(faker_password(1_000_000, 2_000_000).len() <= MAX_FAKER_LEN);
    }
}
