use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn malformed_assignments_are_rejected_at_construction() {
    for (mask, value) in [(0, 0), (0, 1), (0x0F, 0x10), (0x80, 0x81)] {
        assert_eq!(
            MaskedByte::new(7, mask, value),
            Err(PatchError::InvalidMask {
                offset: 7,
                mask,
                value,
            }),
            "invalid bit ownership must not become a typed assignment"
        );
    }
}

#[test]
fn masked_application_preserves_unowned_bits() -> TestResult {
    for mask in 1..=u8::MAX {
        for original in 0..=u8::MAX {
            let desired = original.rotate_left(3) & mask;
            let patch = MaskedByte::new(2, mask, desired)?;
            let result = patch.apply(original);
            assert_eq!(result & mask, desired);
            assert_eq!(result & !mask, original & !mask);
            assert_eq!(patch.with_offset(9).offset(), 9);
            assert_eq!(patch.with_offset(9).value(), desired);
        }
    }
    Ok(())
}

#[test]
fn repeated_equal_assignments_are_idempotent_and_disjoint_bits_coalesce() -> TestResult {
    let mut claims = ByteClaims::new();
    let first = MaskedByte::new(8, 0x0F, 0x05)?;
    claims.merge_atomic("first", &[first])?;
    claims.merge_atomic("same", &[first, first])?;
    claims.merge_atomic("upper", &[MaskedByte::new(8, 0xF0, 0xA0)?])?;
    claims.merge_atomic("earlier", &[MaskedByte::new(1, 0xFF, 0x42)?])?;
    assert_eq!(
        claims.into_claims().collect::<Vec<_>>(),
        [
            ("earlier", MaskedByte::new(1, 0xFF, 0x42)?),
            ("first", MaskedByte::new(8, 0xFF, 0xA5)?),
        ]
    );
    Ok(())
}

#[test]
fn a_late_conflict_preserves_all_existing_claims_and_rejects_the_prefix() -> TestResult {
    let mut claims = ByteClaims::new();
    let original = MaskedByte::new(9, 0x03, 0x01)?;
    claims.merge_atomic("original", &[original])?;
    let result = claims.merge_atomic(
        "failed",
        &[
            MaskedByte::new(1, 0xFF, 0x77)?,
            MaskedByte::new(9, 0x03, 0x02)?,
        ],
    );
    assert_eq!(
        result,
        Err(PatchError::Conflict {
            owner: "failed",
            existing: "original",
            offset: 9,
            mask: 3,
        })
    );
    assert_eq!(
        claims.into_claims().collect::<Vec<_>>(),
        [("original", original)]
    );
    Ok(())
}

#[test]
fn conflicts_within_a_new_batch_leave_no_claims() -> TestResult {
    let mut claims = ByteClaims::new();
    let result = claims.merge_atomic(
        "inconsistent",
        &[
            MaskedByte::new(1, 0x01, 0x01)?,
            MaskedByte::new(1, 0x01, 0x00)?,
        ],
    );
    assert!(matches!(
        result,
        Err(PatchError::Conflict { offset: 1, .. })
    ));
    assert_eq!(claims.into_claims().count(), 0);
    Ok(())
}

#[test]
fn conflicts_report_the_owner_of_the_actual_bit() -> TestResult {
    let mut claims = ByteClaims::new();
    claims.merge_atomic("lower", &[MaskedByte::new(1, 0x01, 0)?])?;
    claims.merge_atomic("upper", &[MaskedByte::new(1, 0x80, 0x80)?])?;
    let result = claims.merge_atomic("conflicting", &[MaskedByte::new(1, 0x80, 0)?]);
    assert_eq!(
        result,
        Err(PatchError::Conflict {
            owner: "conflicting",
            existing: "upper",
            offset: 1,
            mask: 0x80,
        })
    );
    Ok(())
}

#[test]
fn failed_batch_can_be_followed_by_a_valid_assignment() -> TestResult {
    let mut claims = ByteClaims::new();
    let original = MaskedByte::new(1, 0x01, 0x01)?;
    claims.merge_atomic("original", &[original])?;
    let rejected = claims.merge_atomic("failed", &[MaskedByte::new(1, 0x01, 0)?]);
    assert!(rejected.is_err());
    claims.merge_atomic("valid", &[MaskedByte::new(1, 0x02, 0x02)?])?;
    claims.merge_atomic("empty", &[])?;
    assert_eq!(
        claims.into_claims().collect::<Vec<_>>(),
        [("original", MaskedByte::new(1, 0x03, 0x03)?)]
    );
    Ok(())
}
