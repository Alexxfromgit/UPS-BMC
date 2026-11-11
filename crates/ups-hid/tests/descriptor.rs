use ups_hid::{
    MAX_COLLECTION_DEPTH, MAX_FIELDS, ParseError, ReportDescriptor, ReportKind, Usage, units,
};

/// A small, UPS-shaped descriptor exercising the tricky parts: report IDs,
/// nested collections, multi-byte logical max, 1-byte 0xFF logical max,
/// unit + unit exponent, bit fields and constant padding.
#[rustfmt::skip]
const UPS_LIKE: &[u8] = &[
    0x05, 0x84,             // Usage Page (Power Device)
    0x09, 0x04,             // Usage (UPS)
    0xA1, 0x01,             // Collection (Application)
    0x09, 0x24,             //   Usage (PowerSummary)
    0xA1, 0x02,             //   Collection (Logical)
    0x85, 0x01,             //     Report ID (1)
    0x05, 0x85,             //     Usage Page (Battery System)
    0x09, 0x66,             //     Usage (RemainingCapacity)
    0x15, 0x00,             //     Logical Minimum (0)
    0x26, 0xFF, 0x00,       //     Logical Maximum (255)
    0x75, 0x08,             //     Report Size (8)
    0x95, 0x01,             //     Report Count (1)
    0xB1, 0x02,             //     Feature (Data,Var,Abs)
    0x09, 0x68,             //     Usage (RunTimeToEmpty)
    0x27, 0xFF, 0xFF, 0x00, 0x00, // Logical Maximum (65535)
    0x75, 0x10,             //     Report Size (16)
    0x66, 0x01, 0x10,       //     Unit (seconds)
    0xB1, 0x02,             //     Feature (Data,Var,Abs)
    0x85, 0x02,             //     Report ID (2)
    0x09, 0xD0,             //     Usage (ACPresent)
    0x09, 0x44,             //     Usage (Charging)
    0x09, 0x45,             //     Usage (Discharging)
    0x15, 0x00,             //     Logical Minimum (0)
    0x25, 0x01,             //     Logical Maximum (1)
    0x75, 0x01,             //     Report Size (1)
    0x95, 0x03,             //     Report Count (3)
    0x65, 0x00,             //     Unit (none)
    0x81, 0x02,             //     Input (Data,Var,Abs)
    0x95, 0x05,             //     Report Count (5)
    0x81, 0x03,             //     Input (Const) — padding
    0xC0,                   //   End Collection
    0x85, 0x03,             //   Report ID (3)
    0x05, 0x84,             //   Usage Page (Power Device)
    0x09, 0x30,             //   Usage (Voltage)
    0x15, 0x00,             //   Logical Minimum (0)
    0x25, 0xFF,             //   Logical Maximum (0xFF — must read as 255, not -1)
    0x67, 0x21, 0xD1, 0xF0, 0x00, // Unit (volt)
    0x55, 0x07,             //   Unit Exponent (7)
    0x75, 0x08,             //   Report Size (8)
    0x95, 0x01,             //   Report Count (1)
    0xB1, 0x02,             //   Feature (Data,Var,Abs)
    0xC0,                   // End Collection
];

fn parse() -> ReportDescriptor {
    ReportDescriptor::parse(UPS_LIKE).expect("valid descriptor")
}

fn usages_of(d: &ReportDescriptor) -> Vec<Usage> {
    d.fields().iter().map(|f| f.usage).collect()
}

const fn u(page: u16, id: u16) -> Usage {
    Usage::new(page, id)
}

// ---------------------------------------------------------------- basics

#[test]
fn finds_all_data_fields_and_skips_padding() {
    let d = parse();
    assert!(d.uses_report_ids());
    assert_eq!(d.fields().len(), 6);
    assert_eq!(d.report_ids(ReportKind::Feature), [1, 3]);
    assert_eq!(d.report_ids(ReportKind::Input), [2]);
    assert_eq!(d.report_ids(ReportKind::Output), Vec::<u8>::new());
}

#[test]
fn computes_report_lengths() {
    let d = parse();
    assert_eq!(d.report_len(ReportKind::Feature, 1), 3);
    assert_eq!(d.report_len(ReportKind::Input, 2), 1); // 3 bits + 5 padding
    assert_eq!(d.report_len(ReportKind::Feature, 3), 1);
    assert_eq!(d.report_len(ReportKind::Feature, 9), 0);
    assert_eq!(d.max_report_len(ReportKind::Feature), 3);
}

#[test]
fn tracks_offsets_units_and_collections() {
    let d = parse();
    let runtime = d
        .fields()
        .iter()
        .find(|f| f.usage == u(0x85, 0x68))
        .unwrap();
    assert_eq!((runtime.bit_offset, runtime.bit_size), (8, 16));
    assert_eq!(runtime.logical_max, 65_535);
    assert_eq!(runtime.unit, units::SECOND);
    assert_eq!(d.collection_path(runtime), [u(0x84, 0x04), u(0x84, 0x24)]);

    let voltage = d
        .fields()
        .iter()
        .find(|f| f.usage == u(0x84, 0x30))
        .unwrap();
    assert_eq!(voltage.logical_max, 255);
    assert_eq!(voltage.unit, units::VOLT);
    assert_eq!(voltage.unit_exponent, 7);
    assert_eq!(d.collection_path(voltage), [u(0x84, 0x04)]);

    // Collections are stored once and shared by index.
    assert_eq!(d.collections().len(), 2);
    let capacity = &d.fields()[0];
    assert_eq!(capacity.collection, runtime.collection);
}

#[test]
fn decodes_feature_and_input_reports() {
    let d = parse();

    // Feature report 1 payload: capacity 100 %, runtime 0x0E10 = 3600 s.
    let payload = [0x64, 0x10, 0x0E];
    let values: Vec<_> = d
        .fields_in(ReportKind::Feature, 1)
        .map(|f| f.extract(&payload).unwrap())
        .collect();
    assert_eq!(values, [100, 3600]);

    // Input report 2 payload: ACPresent=1, Charging=0, Discharging=1.
    let bits: Vec<_> = d
        .fields_in(ReportKind::Input, 2)
        .map(|f| f.extract(&[0b0000_0101]).unwrap())
        .collect();
    assert_eq!(bits, [1, 0, 1]);

    // Too-short payload yields None rather than garbage.
    let runtime = d.fields_in(ReportKind::Feature, 1).nth(1).unwrap();
    assert_eq!(runtime.extract(&[0x64]), None);
}

// ------------------------------------------------------- value decoding

#[test]
fn sign_extends_signed_fields() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x84, 0x09, 0x36,  // Usage (Temperature)
        0x15, 0x80,              // Logical Minimum (-128)
        0x25, 0x7F,              // Logical Maximum (127)
        0x75, 0x08, 0x95, 0x01,
        0xB1, 0x02,
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    assert!(!d.uses_report_ids());
    let field = &d.fields()[0];
    assert!(field.is_signed());
    assert_eq!(field.extract(&[0xFE]), Some(-2));
    assert_eq!(field.extract(&[0x7F]), Some(127));
}

#[test]
fn extracts_unaligned_and_full_width_fields() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x84,
        0x15, 0x00, 0x26, 0xFF, 0x0F, // 0..4095
        0x75, 0x04, 0x95, 0x01, 0xB1, 0x03, // 4 bits constant padding (no usage)
        0x09, 0x30,
        0x75, 0x0C, 0xB1, 0x02,             // 12-bit field at offset 4
        0x09, 0x31,
        0x17, 0x00, 0x00, 0x00, 0x80,       // Logical Minimum (i32::MIN)
        0x27, 0xFF, 0xFF, 0xFF, 0x7F,       // Logical Maximum (i32::MAX)
        0x75, 0x20, 0xB1, 0x02,             // 32-bit signed field at offset 16
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    let [twelve, wide] = d.fields() else {
        panic!("expected two fields")
    };
    assert_eq!((twelve.bit_offset, twelve.bit_size), (4, 12));
    assert_eq!(twelve.extract(&[0xB0, 0xAB, 0, 0, 0, 0]), Some(0xABB));
    assert_eq!((wide.bit_offset, wide.bit_size), (16, 32));
    assert_eq!(wide.extract(&[0, 0, 0xFE, 0xFF, 0xFF, 0xFF]), Some(-2));
    assert_eq!(
        wide.extract(&[0, 0, 0x00, 0x00, 0x00, 0x80]),
        Some(i64::from(i32::MIN))
    );
}

/// Eaton 5E Gen2 style: read-only values are declared Constant (0x83 =
/// Cnst,Var,Abs,Vol) and must still be decoded; only usage-less constant
/// items are padding. Bytes taken from the real 5E900UD descriptor.
#[test]
fn constant_items_with_usages_are_read_only_data() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x84, 0x09, 0x04, 0xA1, 0x01,       // UPS
        0x09, 0x1A, 0xA1, 0x81,                   //   Input (collection)
        0x85, 0x0E, 0x09, 0x30,                   //   Report 0x0E, Voltage
        0x15, 0x00, 0x27, 0xFF, 0xFF, 0x00, 0x00,
        0x55, 0x07, 0x67, 0x21, 0xD1, 0xF0, 0x00, //   volts, exponent 7
        0x75, 0x10, 0x95, 0x01,
        0xB1, 0x83,                               //   Feature (Cnst,Var,Abs,Vol)
        0x75, 0x08, 0xB1, 0x03,                   //   padding: no usage
        0xC0, 0xC0,
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    let [voltage] = d.fields() else {
        panic!("expected exactly one field")
    };
    assert!(voltage.is_constant());
    assert_eq!(voltage.usage, u(0x84, 0x30));
    assert_eq!(voltage.extract(&[0xE6, 0x00, 0x00]), Some(230));
    assert_eq!(d.report_len(ReportKind::Feature, 0x0E), 3);
}

#[test]
fn fields_wider_than_32_bits_are_parsed_but_not_extracted() {
    let desc = [0x05, 0x84, 0x09, 0x30, 0x75, 0x28, 0x95, 0x01, 0xB1, 0x02];
    let d = ReportDescriptor::parse(&desc).unwrap();
    assert_eq!(d.fields()[0].bit_size, 40);
    assert_eq!(d.fields()[0].extract(&[0xFF; 8]), None);
}

#[test]
fn scales_physical_ranges() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x84, 0x09, 0x35,  // Usage (PercentLoad)
        0x15, 0x00, 0x25, 0x64,  // Logical 0..100
        0x35, 0x00, 0x45, 0x32,  // Physical 0..50
        0x75, 0x08, 0x95, 0x01,
        0xB1, 0x02,
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    assert!((d.fields()[0].scale(40) - 20.0).abs() < f64::EPSILON);
}

#[test]
fn scales_negative_physical_ranges() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x84, 0x09, 0x36,
        0x15, 0x00, 0x25, 0x64,  // Logical 0..100
        0x35, 0xCE, 0x45, 0x32,  // Physical -50..50
        0x75, 0x08, 0x95, 0x01,
        0xB1, 0x02,
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    let f = &d.fields()[0];
    assert_eq!((f.physical_min, f.physical_max), (-50, 50));
    assert!((f.scale(25) - -25.0).abs() < f64::EPSILON);
}

#[test]
fn decodes_unit_exponent_encodings() {
    let exponent = |item: &[u8]| {
        let mut desc = vec![0x05, 0x84, 0x09, 0x30];
        desc.extend_from_slice(item);
        desc.extend_from_slice(&[0x75, 0x08, 0x95, 0x01, 0xB1, 0x02]);
        ReportDescriptor::parse(&desc).unwrap().fields()[0].unit_exponent
    };
    assert_eq!(exponent(&[0x55, 0x07]), 7);
    assert_eq!(exponent(&[0x55, 0x0E]), -2); // nibble, 1 byte
    assert_eq!(exponent(&[0x56, 0x0E, 0x00]), -2); // nibble, 2 bytes
    assert_eq!(exponent(&[0x55, 0xFE]), -2); // full signed byte
    assert_eq!(exponent(&[0x55, 0x08]), -8);
}

// ----------------------------------------------------- usage resolution

#[test]
fn usage_page_after_usage_applies_at_main_item() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x01, 0x09, 0x30,  // Usage Page (Generic Desktop), Usage (0x30)
        0x05, 0x84,              // Usage Page (Power Device) — declared after
        0x75, 0x08, 0x95, 0x01, 0xB1, 0x02,
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    assert_eq!(usages_of(&d), [u(0x84, 0x30)]);
}

#[test]
fn mixed_page_local_sets_keep_earlier_pages() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x85, 0x09, 0x66,  // BatterySystem.RemainingCapacity
        0x05, 0x84, 0x09, 0x30,  // PowerDevice.Voltage
        0x75, 0x08, 0x95, 0x02, 0xB1, 0x02,
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    assert_eq!(usages_of(&d), [u(0x85, 0x66), u(0x84, 0x30)]);
}

#[test]
fn collection_usage_page_is_resolved_too() {
    #[rustfmt::skip]
    let desc = [
        0x09, 0x04, 0x05, 0x84, 0xA1, 0x01,  // Usage before page
        0x09, 0x30, 0x75, 0x08, 0x95, 0x01, 0xB1, 0x02,
        0xC0,
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    assert_eq!(d.collection_path(&d.fields()[0]), [u(0x84, 0x04)]);
}

#[test]
fn extended_usages_keep_their_own_page() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x01,
        0x0B, 0x30, 0x00, 0x84, 0x00,  // Usage (0x0084_0030, extended)
        0x75, 0x08, 0x95, 0x01, 0xB1, 0x02,
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    assert_eq!(usages_of(&d), [u(0x84, 0x30)]);
}

#[test]
fn expands_usage_ranges_in_declaration_order() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x84,
        0x09, 0x01,              // Usage (0x01)
        0x19, 0x10, 0x29, 0x12,  // Usage Minimum (0x10), Maximum (0x12)
        0x75, 0x08, 0x95, 0x05, 0xB1, 0x02,
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    // The fifth field reuses the last usage.
    assert_eq!(
        usages_of(&d),
        [
            u(0x84, 0x01),
            u(0x84, 0x10),
            u(0x84, 0x11),
            u(0x84, 0x12),
            u(0x84, 0x12)
        ]
    );
}

#[test]
fn delimiters_keep_only_the_first_alternative() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x84,
        0xA9, 0x01, 0x09, 0x30, 0x09, 0x31, 0xA9, 0x00,  // Delimiter{30 | 31}
        0x09, 0x32,
        0x75, 0x08, 0x95, 0x02, 0xB1, 0x02,
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    assert_eq!(usages_of(&d), [u(0x84, 0x30), u(0x84, 0x32)]);
}

#[test]
fn array_fields_resolve_selectors() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x84,
        0x19, 0x10, 0x29, 0x12,  // Usage Minimum/Maximum 0x10..0x12
        0x15, 0x01, 0x25, 0x03,  // Logical 1..3 (0 = nothing selected)
        0x75, 0x08, 0x95, 0x02,
        0x81, 0x00,              // Input (Data,Array,Abs)
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    assert_eq!(d.fields().len(), 2);
    let slot = &d.fields()[1];
    assert!(slot.is_array());
    assert_eq!(slot.bit_offset, 8);
    assert_eq!(
        d.array_usages(slot).unwrap(),
        [u(0x84, 0x10), u(0x84, 0x11), u(0x84, 0x12)]
    );
    assert_eq!(d.array_selection(slot, 3), Some(u(0x84, 0x12)));
    assert_eq!(d.array_selection(slot, 0), None);
    assert_eq!(d.array_selection(slot, 9), None);
}

// ------------------------------------------------------ report layout

#[test]
fn same_report_id_has_separate_input_and_feature_layouts() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x84, 0x85, 0x05, 0x75, 0x08, 0x95, 0x01,
        0x09, 0x30, 0x81, 0x02,  // Input #5 at bit 0
        0x09, 0x31, 0xB1, 0x02,  // Feature #5 at bit 0
        0x09, 0x32, 0x81, 0x02,  // Input #5 at bit 8
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    let offsets: Vec<_> = d.fields().iter().map(|f| (f.kind, f.bit_offset)).collect();
    assert_eq!(
        offsets,
        [
            (ReportKind::Input, 0),
            (ReportKind::Feature, 0),
            (ReportKind::Input, 8)
        ]
    );
    assert_eq!(d.report_len(ReportKind::Input, 5), 2);
    assert_eq!(d.report_len(ReportKind::Feature, 5), 1);
}

#[test]
fn push_and_pop_restore_globals() {
    #[rustfmt::skip]
    let desc = [
        0x05, 0x84, 0x75, 0x08, 0x95, 0x01,
        0xA4,                    // Push
        0x75, 0x10,              // Report Size (16)
        0xB4,                    // Pop
        0x09, 0x30, 0xB1, 0x02,  // Voltage feature — size must be back to 8
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    assert_eq!(d.fields()[0].bit_size, 8);
}

#[test]
fn skips_long_items() {
    #[rustfmt::skip]
    let desc = [
        0xFE, 0x02, 0x10, 0xAA, 0xBB,  // Long item, 2 data bytes
        0x05, 0x84, 0x09, 0x30, 0x75, 0x08, 0x95, 0x01, 0xB1, 0x02,
    ];
    let d = ReportDescriptor::parse(&desc).unwrap();
    assert_eq!(usages_of(&d), [u(0x84, 0x30)]);
}

// ------------------------------------------------- malformed / hostile

#[test]
fn rejects_malformed_descriptors() {
    let err = |bytes: &[u8]| ReportDescriptor::parse(bytes).unwrap_err();
    assert_eq!(err(&[0x26, 0xFF]), ParseError::Truncated { offset: 0 });
    assert_eq!(
        err(&[0xFE, 0x05, 0x10, 0x00]),
        ParseError::Truncated { offset: 0 }
    );
    assert_eq!(
        err(&[0x05, 0x84, 0xC0]),
        ParseError::UnbalancedCollection { offset: 2 }
    );
    assert_eq!(
        err(&[0x05, 0x84, 0x09, 0x04, 0xA1, 0x01]),
        ParseError::UnclosedCollection { open: 1 }
    );
    assert_eq!(err(&[0xB4]), ParseError::StackUnderflow { offset: 0 });
    assert_eq!(
        err(&[0x85, 0x00]),
        ParseError::Invalid {
            what: "Report ID",
            offset: 0
        }
    );
}

#[test]
fn rejects_overflowing_report_sizes() {
    // Regression (review R2): these used to panic in debug and wrap in release.
    let huge_size = [
        0x05, 0x84, 0x09, 0x30, 0x77, 0x00, 0x00, 0x00, 0x80, 0x95, 0x03, 0x81, 0x02,
    ];
    assert!(matches!(
        ReportDescriptor::parse(&huge_size),
        Err(ParseError::LimitExceeded {
            what: "Report Size",
            ..
        })
    ));

    let wrapping_padding = [
        0x05, 0x84, 0x09, 0x30, 0x77, 0xFF, 0xFF, 0xFF, 0xFF, 0x95, 0x02, 0x81, 0x03, 0x75, 0x08,
        0x95, 0x02, 0x81, 0x02,
    ];
    assert!(matches!(
        ReportDescriptor::parse(&wrapping_padding),
        Err(ParseError::LimitExceeded { .. })
    ));

    let huge_count = [0x75, 0x20, 0x97, 0xFF, 0xFF, 0xFF, 0xFF, 0x81, 0x03];
    assert!(matches!(
        ReportDescriptor::parse(&huge_count),
        Err(ParseError::LimitExceeded {
            what: "report length",
            ..
        })
    ));
}

#[test]
fn bounds_field_count() {
    // Regression (review R3): a 4 KiB descriptor used to expand to 8 M fields.
    let mut desc = vec![0x05, 0x84, 0x75, 0x01, 0x96, 0x00, 0x10];
    while desc.len() < 4096 {
        desc.extend_from_slice(&[0x81, 0x00]);
    }
    assert!(matches!(
        ReportDescriptor::parse(&desc),
        Err(ParseError::LimitExceeded { what: "fields", .. })
    ));
    const {
        assert!(
            MAX_FIELDS >= 1024,
            "limit must stay generous for real devices"
        );
    }
}

#[test]
fn bounds_collection_depth() {
    let mut desc = vec![0x05, 0x84];
    for _ in 0..=MAX_COLLECTION_DEPTH {
        desc.extend_from_slice(&[0x09, 0x04, 0xA1, 0x02]);
    }
    assert!(matches!(
        ReportDescriptor::parse(&desc),
        Err(ParseError::LimitExceeded {
            what: "collection depth",
            ..
        })
    ));
}

mod property {
    use proptest::prelude::*;
    use ups_hid::{ReportDescriptor, ReportKind};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20_000))]

        /// Any byte string either parses or errors — never panics — and every
        /// parsed field lies inside its report.
        #[test]
        fn never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256),
                        payload in proptest::collection::vec(any::<u8>(), 0..64)) {
            if let Ok(d) = ReportDescriptor::parse(&bytes) {
                for f in d.fields() {
                    let end = u64::from(f.bit_offset) + u64::from(f.bit_size);
                    let len = u64::try_from(d.report_len(f.kind, f.report_id)).unwrap() * 8;
                    prop_assert!(end <= len);
                    let _ = f.extract(&payload);
                    let _ = f.scale(i64::from(i32::MAX));
                    let _ = d.collection_path(f);
                    if f.is_array() {
                        let _ = d.array_selection(f, i64::from(payload.first().copied().unwrap_or(0)));
                    }
                }
                for kind in [ReportKind::Input, ReportKind::Output, ReportKind::Feature] {
                    let _ = d.report_ids(kind);
                }
            }
        }
    }
}
