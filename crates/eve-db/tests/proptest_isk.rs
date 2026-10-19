//! Property-based (`proptest`, >= 10,000 cases) and boundary tests for `eve-db`:
//! - `IskCents` fixed-point currency arithmetic invariants (`checked_add`, `checked_sub`,
//!   `checked_mul_qty`, `from_isk`, `from_f64_isk`, `to_wire_f64`).

use eve_db::IskCents;
use proptest::prelude::*;

fn proptest_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(10_000)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(proptest_cases()))]

    #[test]
    fn prop_isk_cents_fixed_point_invariants(
        a in 0i64..1_000_000_000_000_000i64,
        b in 0i64..1_000_000_000_000_000i64,
        c in 0i64..1_000_000_000_000_000i64,
        qty1 in 0i64..1_000i64,
        qty2 in 0i64..1_000i64,
        neg in -1_000_000_000i64..-1i64,
        whole_isk in 0i64..10_000_000_000_000i64,
        wire_cents in 0i64..10_000_000_000_000i64,
    ) {
        let ia = IskCents(a);
        let ib = IskCents(b);
        let ic = IskCents(c);

        // 1. Commutativity & associativity of checked_add
        let ab = ia.checked_add(ib).expect("bounded positive IskCents addition must succeed");
        let ba = ib.checked_add(ia).expect("bounded positive IskCents addition must succeed");
        prop_assert_eq!(ab, ba);
        prop_assert_eq!(ab.0, a + b);

        let abc_left = ab.checked_add(ic).unwrap();
        let bc = ib.checked_add(ic).unwrap();
        let abc_right = ia.checked_add(bc).unwrap();
        prop_assert_eq!(abc_left, abc_right);

        // 2. Exact additive/subtractive conservation: (a + b) - b == a and (a + b) - a == b
        prop_assert_eq!(ab.checked_sub(ib), Some(ia));
        prop_assert_eq!(ab.checked_sub(ia), Some(ib));
        prop_assert_eq!(ia.checked_sub(ia), Some(IskCents(0)));

        // 3. Underflow protection when subtracting a strictly larger amount
        if a < b {
            prop_assert_eq!(ia.checked_sub(ib), None);
        } else {
            prop_assert_eq!(ia.checked_sub(ib), Some(IskCents(a - b)));
        }

        // 4. Distributivity of checked_mul_qty over quantity addition:
        //    a * (qty1 + qty2) == (a * qty1) + (a * qty2)
        let prod_sum = ia.checked_mul_qty(qty1 + qty2).unwrap();
        let p1 = ia.checked_mul_qty(qty1).unwrap();
        let p2 = ia.checked_mul_qty(qty2).unwrap();
        prop_assert_eq!(Some(prod_sum), p1.checked_add(p2));
        prop_assert_eq!(ia.checked_mul_qty(0), Some(IskCents(0)));
        prop_assert_eq!(ia.checked_mul_qty(1), Some(ia));

        // 5. Negative inputs are strictly rejected across all checked operations
        let ineg = IskCents(neg);
        prop_assert_eq!(ia.checked_add(ineg), None);
        prop_assert_eq!(ineg.checked_add(ia), None);
        prop_assert_eq!(ia.checked_sub(ineg), None);
        prop_assert_eq!(ineg.checked_sub(ia), None);
        prop_assert_eq!(ia.checked_mul_qty(neg), None);
        prop_assert_eq!(ineg.checked_mul_qty(qty1), None);

        // 6. Exact whole-ISK scaling and IEEE-754 f64 wire round-trip conservation
        let from_whole = IskCents::from_isk(whole_isk);
        prop_assert_eq!(from_whole.0, whole_isk * 100);

        let w = IskCents(wire_cents);
        let roundtripped = IskCents::from_f64_isk(w.to_wire_f64());
        prop_assert_eq!(roundtripped, w);
    }
}

#[test]
fn test_isk_cents_overflow_and_non_finite_f64_bounds() {
    assert_eq!(IskCents(i64::MAX).checked_add(IskCents(1)), None);
    assert_eq!(IskCents(i64::MAX / 2 + 10).checked_mul_qty(2), None);
    assert_eq!(IskCents::from_f64_isk(f64::NAN), IskCents(0));
    assert_eq!(IskCents::from_f64_isk(f64::INFINITY), IskCents(0));
    assert_eq!(IskCents::from_f64_isk(f64::NEG_INFINITY), IskCents(0));
    assert_eq!(IskCents::from_f64_isk(1e30), IskCents(i64::MAX / 2));
    assert_eq!(IskCents::from_f64_isk(-1e30), IskCents(i64::MIN / 2));
}
