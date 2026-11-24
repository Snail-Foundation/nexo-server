//! Money, exchange, and included-IVA rules for web billing.
//!
//! The restaurant stores menu prices in the business base currency and, when
//! IVA is enabled, those prices already include the tax. The functions here
//! are pure so the web cashier and its tests use the same arithmetic.

use crate::error::AppError;

/// Exchange rates are stored as micros: 36.50 VES/USD is `36_500_000`.
pub const RATE_SCALE: i64 = 1_000_000;

/// Returns the secondary currency for a base currency.
pub fn secondary_currency(base: &str) -> &'static str {
    if base == "USD" { "VES" } else { "USD" }
}

/// Validates an exchange rate in micros.
pub fn valid_rate(rate_micros: i64) -> Result<i64, AppError> {
    if rate_micros <= 0 {
        return Err(AppError::Validation(
            "the exchange rate must be greater than zero".to_owned(),
        ));
    }
    Ok(rate_micros)
}

/// Converts an amount in `currency` to the base currency, rounding half up.
pub fn to_base_minor(
    amount_minor: i64,
    currency: &str,
    base: &str,
    rate_micros: i64,
) -> Result<i64, AppError> {
    if amount_minor < 0 {
        return Err(AppError::Validation(
            "amounts cannot be negative".to_owned(),
        ));
    }
    if currency == base {
        return Ok(amount_minor);
    }

    let rate = valid_rate(rate_micros)?;
    if currency == "VES" && base == "USD" {
        round_div(
            i128::from(amount_minor) * i128::from(RATE_SCALE),
            i128::from(rate),
        )
    } else if currency == "USD" && base == "VES" {
        round_div(
            i128::from(amount_minor) * i128::from(rate),
            i128::from(RATE_SCALE),
        )
    } else {
        Err(AppError::Validation(
            "the payment currency is not supported by this business".to_owned(),
        ))
    }
}

/// Returns the amount in `currency` needed to cover a base amount, rounding up.
pub fn amount_covering_base(
    base_amount_minor: i64,
    currency: &str,
    base: &str,
    rate_micros: i64,
) -> Result<i64, AppError> {
    if base_amount_minor < 0 {
        return Err(AppError::Validation(
            "amounts cannot be negative".to_owned(),
        ));
    }
    if currency == base || base_amount_minor == 0 {
        return Ok(base_amount_minor);
    }

    let rate = valid_rate(rate_micros)?;
    if currency == "VES" && base == "USD" {
        ceil_div(
            i128::from(base_amount_minor) * i128::from(rate),
            i128::from(RATE_SCALE),
        )
    } else if currency == "USD" && base == "VES" {
        ceil_div(
            i128::from(base_amount_minor) * i128::from(RATE_SCALE),
            i128::from(rate),
        )
    } else {
        Err(AppError::Validation(
            "the payment currency is not supported by this business".to_owned(),
        ))
    }
}

/// Returns the IVA portion already included in a final price.
pub fn included_tax_minor(total_minor: i64, rate_basis_points: i64) -> Result<i64, AppError> {
    if total_minor < 0 {
        return Err(AppError::Validation(
            "the sale total cannot be negative".to_owned(),
        ));
    }
    if !(0..=10_000).contains(&rate_basis_points) {
        return Err(AppError::Validation(
            "the IVA rate is out of range".to_owned(),
        ));
    }
    if rate_basis_points == 0 || total_minor == 0 {
        return Ok(0);
    }

    let denominator = i128::from(10_000 + rate_basis_points);
    let numerator = i128::from(total_minor) * i128::from(rate_basis_points);
    let tax = (numerator + denominator / 2) / denominator;

    i64::try_from(tax).map_err(|_| AppError::Validation("the IVA amount is too large".to_owned()))
}

/// Parses a user-entered amount such as `12`, `12.50`, or `12,50` into minor
/// units. Only two decimal places are accepted.
pub fn parse_amount_minor(input: &str) -> Result<i64, AppError> {
    let value = input.trim().replace(',', ".");
    if value.is_empty() {
        return Err(AppError::Validation("enter an amount".to_owned()));
    }

    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (value.as_str(), ""),
    };

    if whole.is_empty() && fraction.is_empty() {
        return Err(AppError::Validation("enter an amount".to_owned()));
    }
    if fraction.len() > 2 || !fraction.chars().all(|character| character.is_ascii_digit()) {
        return Err(AppError::Validation(
            "amounts can have at most two decimals".to_owned(),
        ));
    }
    if !whole.chars().all(|character| character.is_ascii_digit()) {
        return Err(AppError::Validation("invalid amount".to_owned()));
    }

    let whole_minor = whole
        .parse::<i64>()
        .map_err(|_| AppError::Validation("the amount is too large".to_owned()))?
        .checked_mul(100)
        .ok_or_else(|| AppError::Validation("the amount is too large".to_owned()))?;
    let fraction_minor = match fraction.len() {
        0 => 0,
        1 => {
            fraction
                .parse::<i64>()
                .map_err(|_| AppError::Validation("invalid amount".to_owned()))?
                * 10
        }
        _ => fraction
            .parse::<i64>()
            .map_err(|_| AppError::Validation("invalid amount".to_owned()))?,
    };

    whole_minor
        .checked_add(fraction_minor)
        .ok_or_else(|| AppError::Validation("the amount is too large".to_owned()))
}

/// Parses an exchange rate such as `36.50` into micros.
pub fn parse_rate_micros(input: &str) -> Result<i64, AppError> {
    let value = input.trim().replace(',', ".");
    if value.is_empty() {
        return Err(AppError::Validation(
            "the exchange rate is required for payments in the secondary currency".to_owned(),
        ));
    }

    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (value.as_str(), ""),
    };
    if fraction.len() > 6 || !fraction.chars().all(|character| character.is_ascii_digit()) {
        return Err(AppError::Validation(
            "the exchange rate can have at most six decimals".to_owned(),
        ));
    }
    if !whole.chars().all(|character| character.is_ascii_digit()) {
        return Err(AppError::Validation("invalid exchange rate".to_owned()));
    }

    let whole_micros = whole
        .parse::<i64>()
        .map_err(|_| AppError::Validation("the exchange rate is too large".to_owned()))?
        .checked_mul(RATE_SCALE)
        .ok_or_else(|| AppError::Validation("the exchange rate is too large".to_owned()))?;
    let fraction_text = format!("{fraction:0<6}");
    let fraction_micros = if fraction.is_empty() {
        0
    } else {
        fraction_text
            .parse::<i64>()
            .map_err(|_| AppError::Validation("invalid exchange rate".to_owned()))?
    };

    let rate = whole_micros
        .checked_add(fraction_micros)
        .ok_or_else(|| AppError::Validation("the exchange rate is too large".to_owned()))?;
    valid_rate(rate)
}

/// Formats micros as a decimal rate without trailing zeroes.
pub fn format_rate_micros(micros: i64) -> String {
    let whole = micros / RATE_SCALE;
    let fraction = micros % RATE_SCALE;
    if fraction == 0 {
        return whole.to_string();
    }

    let formatted = format!("{whole}.{fraction:06}");
    formatted.trim_end_matches('0').to_owned()
}

fn round_div(numerator: i128, denominator: i128) -> Result<i64, AppError> {
    if denominator <= 0 {
        return Err(AppError::Validation(
            "the exchange rate must be greater than zero".to_owned(),
        ));
    }
    let value = (numerator + denominator / 2) / denominator;
    i64::try_from(value).map_err(|_| AppError::Validation("the amount is too large".to_owned()))
}

fn ceil_div(numerator: i128, denominator: i128) -> Result<i64, AppError> {
    if denominator <= 0 {
        return Err(AppError::Validation(
            "the exchange rate must be greater than zero".to_owned(),
        ));
    }
    let value = (numerator + denominator - 1) / denominator;
    i64::try_from(value).map_err(|_| AppError::Validation("the amount is too large".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secondary_currency_flips_with_the_base() {
        assert_eq!(secondary_currency("USD"), "VES");
        assert_eq!(secondary_currency("VES"), "USD");
    }

    #[test]
    fn converts_ves_to_usd_rounding_half_up() {
        assert_eq!(to_base_minor(3_650, "VES", "USD", 36_500_000).unwrap(), 100);
        assert_eq!(to_base_minor(3_649, "VES", "USD", 36_500_000).unwrap(), 100);
        assert_eq!(to_base_minor(3_600, "VES", "USD", 36_500_000).unwrap(), 99);
    }

    #[test]
    fn base_currency_is_unchanged() {
        assert_eq!(to_base_minor(1_250, "USD", "USD", 0).unwrap(), 1_250);
        assert_eq!(amount_covering_base(1_250, "USD", "USD", 0).unwrap(), 1_250);
    }

    #[test]
    fn covers_a_base_amount_rounding_up() {
        assert_eq!(
            amount_covering_base(100, "VES", "USD", 36_500_000).unwrap(),
            3_650
        );
        assert_eq!(
            amount_covering_base(99, "VES", "USD", 36_500_000).unwrap(),
            3_614
        );
    }

    #[test]
    fn included_tax_is_extracted_from_the_total() {
        assert_eq!(included_tax_minor(1_160, 1_600).unwrap(), 160);
        assert_eq!(included_tax_minor(100, 0).unwrap(), 0);
        assert_eq!(included_tax_minor(0, 1_600).unwrap(), 0);
        assert!(included_tax_minor(100, 10_001).is_err());
    }

    #[test]
    fn parses_amounts_with_one_or_two_decimals() {
        assert_eq!(parse_amount_minor("12").unwrap(), 1_200);
        assert_eq!(parse_amount_minor("12.5").unwrap(), 1_250);
        assert_eq!(parse_amount_minor("12,50").unwrap(), 1_250);
        assert_eq!(parse_amount_minor(" 0.05 ").unwrap(), 5);
        assert!(parse_amount_minor("1.234").is_err());
        assert!(parse_amount_minor("abc").is_err());
        assert!(parse_amount_minor("").is_err());
    }

    #[test]
    fn parses_and_formats_rates() {
        assert_eq!(parse_rate_micros("36.5").unwrap(), 36_500_000);
        assert_eq!(parse_rate_micros("36,500001").unwrap(), 36_500_001);
        assert_eq!(format_rate_micros(36_500_000), "36.5");
        assert_eq!(format_rate_micros(36_000_000), "36");
        assert!(parse_rate_micros("0").is_err());
        assert!(parse_rate_micros("abc").is_err());
    }
}
