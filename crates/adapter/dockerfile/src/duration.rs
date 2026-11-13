//! `HEALTHCHECK --interval=1m30s`의 시간. Go의 `time.ParseDuration`과 같은 형식이다.
//!
//! 부호(선택) 뒤에 `숫자[.숫자]단위`가 하나 이상 온다. 단위는 `ns`, `us`(`µs`, `μs`), `ms`, `s`, `m`, `h`다.
//! 단위 없는 `0`만 예외다. 결과는 나노초이고 `i64`를 넘으면 오류다.

/// 나노초로 읽는다.
pub fn parse(s: &str) -> Result<i64, String> {
    let bad = || format!("invalid duration {s:?}");
    let (neg, mut rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if rest == "0" {
        return Ok(0);
    }
    if rest.is_empty() {
        return Err(bad());
    }
    let mut total: u128 = 0;
    while !rest.is_empty() {
        let int_len = rest.bytes().take_while(u8::is_ascii_digit).count();
        let (int, after) = rest.split_at(int_len);
        let (frac, after) = match after.strip_prefix('.') {
            Some(a) => {
                let n = a.bytes().take_while(u8::is_ascii_digit).count();
                a.split_at(n)
            }
            None => ("", after),
        };
        if int.is_empty() && frac.is_empty() {
            return Err(bad());
        }
        let unit_len = after
            .char_indices()
            .find(|&(_, c)| c == '.' || c.is_ascii_digit())
            .map_or(after.len(), |(i, _)| i);
        let (unit, after) = after.split_at(unit_len);
        let scale: u128 = match unit {
            "ns" => 1,
            "us" | "µs" | "μs" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            "" => return Err(format!("missing unit in duration {s:?}")),
            _ => return Err(format!("unknown unit {unit:?} in duration {s:?}")),
        };
        let int: u128 = if int.is_empty() {
            0
        } else {
            int.parse().map_err(|_| bad())?
        };
        let mut value = int.checked_mul(scale).ok_or_else(bad)?;
        // 소수 부분: 단위 안에서 버림(Go와 같다). 자릿수는 나노초보다 잘게 볼 필요가 없다.
        let mut place = scale;
        for d in frac.bytes().take(30) {
            place /= 10;
            value += u128::from(d - b'0') * place;
        }
        total = total.checked_add(value).ok_or_else(bad)?;
        if total > i64::MAX as u128 + 1 {
            return Err(bad());
        }
        rest = after;
    }
    if neg {
        i64::try_from(total).map(|v| -v).or_else(|_| {
            (total == i64::MAX as u128 + 1)
                .then_some(i64::MIN)
                .ok_or_else(bad)
        })
    } else {
        i64::try_from(total).map_err(|_| bad())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_durations() {
        assert_eq!(parse("0").unwrap(), 0);
        assert_eq!(parse("30s").unwrap(), 30_000_000_000);
        assert_eq!(parse("1m30s").unwrap(), 90_000_000_000);
        assert_eq!(parse("1.5h").unwrap(), 5_400_000_000_000);
        assert_eq!(parse(".5s").unwrap(), 500_000_000);
        assert_eq!(parse("1µs").unwrap(), 1_000);
        assert_eq!(parse("10ms").unwrap(), 10_000_000);
        assert_eq!(parse("-1s").unwrap(), -1_000_000_000);
        assert_eq!(parse("2562047h47m16.854775807s").unwrap(), i64::MAX);
        for bad in [
            "",
            "s",
            "1",
            "1x",
            "1.s.",
            ".s",
            "-",
            "9999999999999h",
            "1s2",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
