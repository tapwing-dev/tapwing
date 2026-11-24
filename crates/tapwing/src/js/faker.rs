//! Native Rust `faker` bindings matching Maestro/Datafaker APIs.

use anyhow::Result;
use rquickjs::function::Opt;
use rquickjs::{CatchResultExt, Ctx, Function, Object};

use crate::random;

use super::{catch_js_error, caught_to_anyhow};

/// Upper bound on character/digit counts accepted from JS `faker.*` calls,
/// matching `random::MAX_FAKER_LEN` to prevent runaway allocations.
pub(super) const MAX_FAKER_LEN: i32 = 65_536;

/// Upper bound on word counts for `faker.lorem().words(n)` / `sentence(n)`.
const MAX_FAKER_WORDS: i32 = 4_096;

/// Upper bound on sentence counts for `faker.lorem().paragraph(n)`.
pub(super) const MAX_FAKER_PARAGRAPH_SENTENCES: i32 = 1_000;

/// Installs native Rust `faker` providers matching Maestro/Datafaker APIs.
pub(super) fn install_faker<'js>(ctx: &Ctx<'js>) -> Result<()> {
    let faker = Object::new(ctx.clone()).map_err(|e| catch_js_error(ctx, e))?;
    // Helper JS factory that captures the provider object in a normal GC-traced
    // JS closure (avoiding `Persistent` reference cycles on runtime teardown).
    let make_getter: Function<'js> =
        ctx.eval("(function(obj) { return function() { return obj; }; })").catch(ctx).map_err(caught_to_anyhow)?;

    let make_provider = |ctx: &Ctx<'js>, methods: Vec<(&str, Function<'js>)>| -> Result<Function<'js>> {
        let obj = Object::new(ctx.clone()).map_err(|e| catch_js_error(ctx, e))?;
        for (name, func) in methods {
            obj.set(name, func).map_err(|e| catch_js_error(ctx, e))?;
        }
        make_getter.call::<_, Function<'js>>((obj,)).catch(ctx).map_err(caught_to_anyhow)
    };

    // faker.name()
    let name_fn = make_provider(
        ctx,
        vec![
            ("firstName", Function::new(ctx.clone(), random::faker_first_name).map_err(|e| catch_js_error(ctx, e))?),
            ("lastName", Function::new(ctx.clone(), random::faker_last_name).map_err(|e| catch_js_error(ctx, e))?),
            ("fullName", Function::new(ctx.clone(), random::faker_full_name).map_err(|e| catch_js_error(ctx, e))?),
            ("name", Function::new(ctx.clone(), random::faker_full_name).map_err(|e| catch_js_error(ctx, e))?),
            ("username", Function::new(ctx.clone(), random::faker_username).map_err(|e| catch_js_error(ctx, e))?),
            ("title", Function::new(ctx.clone(), random::faker_title).map_err(|e| catch_js_error(ctx, e))?),
        ],
    )?;
    faker.set("name", name_fn).map_err(|e| catch_js_error(ctx, e))?;

    // faker.internet()
    let internet_fn = make_provider(
        ctx,
        vec![
            ("emailAddress", Function::new(ctx.clone(), random::faker_email).map_err(|e| catch_js_error(ctx, e))?),
            ("safeEmailAddress", Function::new(ctx.clone(), random::faker_email).map_err(|e| catch_js_error(ctx, e))?),
            ("domainName", Function::new(ctx.clone(), random::faker_domain_name).map_err(|e| catch_js_error(ctx, e))?),
            ("url", Function::new(ctx.clone(), random::faker_url).map_err(|e| catch_js_error(ctx, e))?),
            (
                "password",
                Function::new(ctx.clone(), |min: Opt<i32>, max: Opt<i32>| {
                    let lo = min.0.unwrap_or(8).clamp(1, MAX_FAKER_LEN);
                    let hi = max.0.unwrap_or(16).clamp(lo, MAX_FAKER_LEN);
                    random::faker_password(lo as usize, hi as usize)
                })
                .map_err(|e| catch_js_error(ctx, e))?,
            ),
            ("ipV4Address", Function::new(ctx.clone(), random::faker_ipv4).map_err(|e| catch_js_error(ctx, e))?),
            ("uuid", Function::new(ctx.clone(), random::faker_uuid).map_err(|e| catch_js_error(ctx, e))?),
        ],
    )?;
    faker.set("internet", internet_fn).map_err(|e| catch_js_error(ctx, e))?;

    // faker.address()
    let address_fn = make_provider(
        ctx,
        vec![
            ("city", Function::new(ctx.clone(), random::faker_city).map_err(|e| catch_js_error(ctx, e))?),
            ("cityName", Function::new(ctx.clone(), random::faker_city).map_err(|e| catch_js_error(ctx, e))?),
            ("country", Function::new(ctx.clone(), random::faker_country).map_err(|e| catch_js_error(ctx, e))?),
            (
                "countryCode",
                Function::new(ctx.clone(), random::faker_country_code).map_err(|e| catch_js_error(ctx, e))?,
            ),
            ("state", Function::new(ctx.clone(), random::faker_state).map_err(|e| catch_js_error(ctx, e))?),
            ("streetName", Function::new(ctx.clone(), random::faker_street_name).map_err(|e| catch_js_error(ctx, e))?),
            (
                "streetAddress",
                Function::new(ctx.clone(), random::faker_street_address).map_err(|e| catch_js_error(ctx, e))?,
            ),
            ("zipCode", Function::new(ctx.clone(), random::faker_zip_code).map_err(|e| catch_js_error(ctx, e))?),
            (
                "fullAddress",
                Function::new(ctx.clone(), random::faker_full_address).map_err(|e| catch_js_error(ctx, e))?,
            ),
        ],
    )?;
    faker.set("address", address_fn).map_err(|e| catch_js_error(ctx, e))?;

    // faker.color()
    let color_fn = make_provider(
        ctx,
        vec![
            ("name", Function::new(ctx.clone(), random::faker_color_name).map_err(|e| catch_js_error(ctx, e))?),
            ("hex", Function::new(ctx.clone(), random::faker_color_hex).map_err(|e| catch_js_error(ctx, e))?),
        ],
    )?;
    faker.set("color", color_fn).map_err(|e| catch_js_error(ctx, e))?;

    // faker.number()
    let number_fn = make_provider(
        ctx,
        vec![
            (
                "digit",
                Function::new(ctx.clone(), || random::faker_number_digits(1)).map_err(|e| catch_js_error(ctx, e))?,
            ),
            (
                "digits",
                Function::new(ctx.clone(), |n: Opt<i32>| {
                    random::faker_number_digits(n.0.unwrap_or(1).clamp(1, MAX_FAKER_LEN) as usize)
                })
                .map_err(|e| catch_js_error(ctx, e))?,
            ),
            (
                "randomNumber",
                Function::new(ctx.clone(), |digits: Opt<i32>| {
                    random::faker_random_number(digits.0.unwrap_or(8).clamp(1, 18) as usize)
                })
                .map_err(|e| catch_js_error(ctx, e))?,
            ),
            (
                "numberBetween",
                Function::new(ctx.clone(), |min: i64, max: i64| random::faker_number_between(min, max))
                    .map_err(|e| catch_js_error(ctx, e))?,
            ),
            (
                "positive",
                Function::new(ctx.clone(), || random::faker_number_between(1, 100_000))
                    .map_err(|e| catch_js_error(ctx, e))?,
            ),
            (
                "negative",
                Function::new(ctx.clone(), || -random::faker_number_between(1, 100_000))
                    .map_err(|e| catch_js_error(ctx, e))?,
            ),
            (
                "randomDouble",
                Function::new(ctx.clone(), |dec: i32, min: i64, max: i64| {
                    random::faker_random_double(dec.clamp(0, 9) as usize, min, max)
                })
                .map_err(|e| catch_js_error(ctx, e))?,
            ),
        ],
    )?;
    faker.set("number", number_fn).map_err(|e| catch_js_error(ctx, e))?;

    // faker.text()
    let text_fn = make_provider(
        ctx,
        vec![(
            "text",
            Function::new(ctx.clone(), |len: Opt<i32>| {
                random::faker_text(len.0.unwrap_or(8).clamp(1, MAX_FAKER_LEN) as usize)
            })
            .map_err(|e| catch_js_error(ctx, e))?,
        )],
    )?;
    faker.set("text", text_fn).map_err(|e| catch_js_error(ctx, e))?;

    // faker.lorem()
    let lorem_fn = make_provider(
        ctx,
        vec![
            ("word", Function::new(ctx.clone(), random::faker_word).map_err(|e| catch_js_error(ctx, e))?),
            (
                "words",
                Function::new(ctx.clone(), |n: Opt<i32>| {
                    random::faker_words(n.0.unwrap_or(3).clamp(1, MAX_FAKER_WORDS) as usize)
                })
                .map_err(|e| catch_js_error(ctx, e))?,
            ),
            (
                "sentence",
                Function::new(ctx.clone(), |n: Opt<i32>| {
                    random::faker_sentence(n.0.unwrap_or(6).clamp(1, MAX_FAKER_WORDS) as usize)
                })
                .map_err(|e| catch_js_error(ctx, e))?,
            ),
            (
                "paragraph",
                Function::new(ctx.clone(), |n: Opt<i32>| {
                    let count = n.0.unwrap_or(3).clamp(1, MAX_FAKER_PARAGRAPH_SENTENCES) as usize;
                    (0..count).map(|_| random::faker_sentence(6)).collect::<Vec<_>>().join(" ")
                })
                .map_err(|e| catch_js_error(ctx, e))?,
            ),
            (
                "characters",
                Function::new(ctx.clone(), |n: Opt<i32>| {
                    random::faker_text(n.0.unwrap_or(10).clamp(1, MAX_FAKER_LEN) as usize)
                })
                .map_err(|e| catch_js_error(ctx, e))?,
            ),
        ],
    )?;
    faker.set("lorem", lorem_fn).map_err(|e| catch_js_error(ctx, e))?;

    // faker.phoneNumber()
    let phone_fn = make_provider(
        ctx,
        vec![
            (
                "phoneNumber",
                Function::new(ctx.clone(), random::faker_phone_number).map_err(|e| catch_js_error(ctx, e))?,
            ),
            ("cellPhone", Function::new(ctx.clone(), random::faker_phone_number).map_err(|e| catch_js_error(ctx, e))?),
        ],
    )?;
    faker.set("phoneNumber", phone_fn).map_err(|e| catch_js_error(ctx, e))?;

    // faker.company()
    let company_fn = make_provider(
        ctx,
        vec![("name", Function::new(ctx.clone(), random::faker_company).map_err(|e| catch_js_error(ctx, e))?)],
    )?;
    faker.set("company", company_fn).map_err(|e| catch_js_error(ctx, e))?;

    // faker.commerce()
    let commerce_fn = make_provider(
        ctx,
        vec![
            (
                "productName",
                Function::new(ctx.clone(), random::faker_product_name).map_err(|e| catch_js_error(ctx, e))?,
            ),
            ("price", Function::new(ctx.clone(), random::faker_price).map_err(|e| catch_js_error(ctx, e))?),
        ],
    )?;
    faker.set("commerce", commerce_fn).map_err(|e| catch_js_error(ctx, e))?;

    // faker.bool()
    let bool_fn = make_provider(
        ctx,
        vec![("bool", Function::new(ctx.clone(), random::faker_bool).map_err(|e| catch_js_error(ctx, e))?)],
    )?;
    faker.set("bool", bool_fn).map_err(|e| catch_js_error(ctx, e))?;

    // faker.idNumber()
    let id_fn = make_provider(
        ctx,
        vec![("valid", Function::new(ctx.clone(), random::faker_uuid).map_err(|e| catch_js_error(ctx, e))?)],
    )?;
    faker.set("idNumber", id_fn).map_err(|e| catch_js_error(ctx, e))?;

    ctx.globals().set("faker", faker).map_err(|e| catch_js_error(ctx, e))?;

    // Datafaker `faker.expression('#{name.firstName} #{name.lastName}')` helper
    // wired directly to our native Rust providers (supports both space- and
    // comma-separated arguments such as `#{number.numberBetween '1' '10'}`).
    ctx.eval::<(), _>(
        r#"
        faker.expression = function(expr) {
            return String(expr).replace(/#\{([a-zA-Z0-9_]+)\.([a-zA-Z0-9_]+)(?:\s+([^}]*))?\}/g, function(m, prov, fn, rawArgs) {
                var p = faker[prov];
                if (typeof p === 'function') {
                    var inst = p();
                    if (inst && typeof inst[fn] === 'function') {
                        if (rawArgs && rawArgs.trim().length > 0) {
                            var tokens = rawArgs.match(/'[^']*'|"[^"]*"|[^,\s]+/g) || [];
                            var args = tokens.map(function(a) {
                                var t = a.trim().replace(/^['"]|['"]$/g, '');
                                var n = Number(t);
                                return (!isNaN(n) && t !== '') ? n : t;
                            });
                            return String(inst[fn].apply(inst, args));
                        }
                        return String(inst[fn]());
                    }
                }
                return m;
            });
        };
        "#,
    )
    .catch(ctx)
    .map_err(caught_to_anyhow)?;

    Ok(())
}
