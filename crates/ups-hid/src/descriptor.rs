//! HID 1.11 report descriptor parser (§6.2.2).
//!
//! The parser is defensive: descriptors come from external hardware, so every
//! size is bounded by the limits below and all arithmetic is checked. A hostile
//! or corrupt descriptor yields a [`ParseError`], never a panic or an unbounded
//! allocation.

use serde::Serialize;

/// Upper bound on data fields in one descriptor.
pub const MAX_FIELDS: usize = 16_384;
/// Upper bound on Collection items in one descriptor.
pub const MAX_COLLECTIONS: usize = 4_096;
/// Upper bound on collection nesting depth.
pub const MAX_COLLECTION_DEPTH: usize = 32;
/// Upper bound on usages in one local item set (including expanded ranges).
pub const MAX_LOCAL_USAGES: usize = 4_096;
/// Upper bound on usages stored for all array fields together.
pub const MAX_ARRAY_USAGES_TOTAL: usize = 65_536;
/// Upper bound on a single Report Size, in bits.
pub const MAX_REPORT_SIZE_BITS: u32 = 1_024;
/// Upper bound on a single report's length, in bits (8 KiB).
pub const MAX_REPORT_BITS: u32 = 8 * 8_192;

/// Which kind of report a field belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum ReportKind {
    Input,
    Output,
    Feature,
}

/// A fully qualified HID usage (usage page + usage ID).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Usage {
    pub page: u16,
    pub id: u16,
}

impl Usage {
    #[must_use]
    pub const fn new(page: u16, id: u16) -> Self {
        Self { page, id }
    }

    /// Splits a 32-bit "extended" usage (page in the high 16 bits).
    #[must_use]
    pub const fn from_extended(value: u32) -> Self {
        Self {
            page: high16(value),
            id: low16(value),
        }
    }
}

/// One Collection item, with the usages of all enclosing collections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Collection {
    /// Collection type (0 = Physical, 1 = Application, 2 = Logical, ...).
    pub kind: u8,
    /// Usages from the outermost collection down to this one (inclusive).
    pub path: Vec<Usage>,
}

/// Bit length of one report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReportLayout {
    pub kind: ReportKind,
    pub report_id: u8,
    /// Total length in bits, padding included, report ID byte excluded.
    pub bits: u32,
}

impl ReportLayout {
    /// Payload length in bytes (report ID byte excluded).
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        usize::try_from(self.bits.div_ceil(8)).unwrap_or(usize::MAX)
    }
}

/// One data field inside an input, output or feature report.
///
/// * **Variable** fields hold a value for [`Field::usage`].
/// * **Array** fields (see [`Field::is_array`]) hold a *selector*: the raw
///   value minus `logical_min` indexes the usage list returned by
///   [`ReportDescriptor::array_usages`]. Use
///   [`ReportDescriptor::array_selection`] to resolve it; `usage` is only the
///   first usage of that list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Field {
    pub kind: ReportKind,
    /// Report ID (0 when the device does not use report IDs).
    pub report_id: u8,
    /// Bit offset inside the report data, *excluding* the report ID byte.
    pub bit_offset: u32,
    pub bit_size: u32,
    pub usage: Usage,
    /// Index of the innermost enclosing collection in
    /// [`ReportDescriptor::collections`], if any.
    pub collection: Option<usize>,
    /// For array fields: index of the usage list (see
    /// [`ReportDescriptor::array_usages`]).
    pub array_usages: Option<usize>,
    pub logical_min: i64,
    pub logical_max: i64,
    pub physical_min: i64,
    pub physical_max: i64,
    /// Raw HID unit code (see [`crate::units`]).
    pub unit: u32,
    /// Unit exponent (power of ten), as declared by the device. Devices are
    /// often inconsistent here, so it is *not* applied automatically.
    pub unit_exponent: i8,
    /// Main item flags (bit 0 = constant, bit 1 = variable, bit 2 = relative, ...).
    pub flags: u32,
}

impl Field {
    /// Variable (as opposed to array) field.
    #[must_use]
    pub fn is_variable(&self) -> bool {
        self.flags & 0x02 != 0
    }

    /// Array field: its value is a usage selector, not a quantity.
    #[must_use]
    pub fn is_array(&self) -> bool {
        !self.is_variable()
    }

    /// Constant (read-only) field: the host can't change it.
    #[must_use]
    pub fn is_constant(&self) -> bool {
        self.flags & 0x01 != 0
    }

    /// Values are two's complement when the logical minimum is negative.
    #[must_use]
    pub fn is_signed(&self) -> bool {
        self.logical_min < 0
    }

    /// Extracts the logical value of this field from report data.
    ///
    /// `data` must be the report payload *without* the leading report ID byte.
    /// Returns `None` if the data is too short or the field is wider than 32 bits.
    #[must_use]
    pub fn extract(&self, data: &[u8]) -> Option<i64> {
        if self.bit_size == 0 || self.bit_size > 32 {
            return None;
        }
        let mut value: u64 = 0;
        for b in 0..self.bit_size {
            let bit = self.bit_offset.checked_add(b)?;
            let byte = *data.get(usize::try_from(bit / 8).ok()?)?;
            if (byte >> (bit % 8)) & 1 == 1 {
                value |= 1 << b;
            }
        }
        // At most 32 bits are set, so this always fits.
        let mut value = i64::try_from(value).ok()?;
        if self.is_signed() && value & (1 << (self.bit_size - 1)) != 0 {
            value -= 1 << self.bit_size;
        }
        Some(value)
    }

    /// Writes `value` into this field's bits of `data` (report payload
    /// without the report ID byte), leaving all other bits untouched.
    ///
    /// Returns `false`, without changing anything, if the value is outside
    /// the logical range, the field is wider than 32 bits or `data` is too short.
    #[must_use]
    pub fn insert(&self, data: &mut [u8], value: i64) -> bool {
        if self.bit_size == 0
            || self.bit_size > 32
            || value < self.logical_min
            || value > self.logical_max
        {
            return false;
        }
        let Some(end) = self.bit_offset.checked_add(self.bit_size) else {
            return false;
        };
        if usize::try_from(end.div_ceil(8)).map_or(true, |len| len > data.len()) {
            return false;
        }
        // Two's complement in `bit_size` bits (masking handles negatives).
        let bits = value.cast_unsigned() & ((1u64 << self.bit_size) - 1);
        for b in 0..self.bit_size {
            let bit = self.bit_offset + b;
            let byte = &mut data[(bit / 8) as usize];
            let mask = 1u8 << (bit % 8);
            if (bits >> b) & 1 == 1 {
                *byte |= mask;
            } else {
                *byte &= !mask;
            }
        }
        true
    }

    /// Maps a logical value onto the physical range when the descriptor
    /// declares one. The unit exponent is deliberately *not* applied.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // values are at most 33 bits wide
    pub fn scale(&self, logical: i64) -> f64 {
        let undefined_physical = self.physical_min == 0 && self.physical_max == 0;
        let identity =
            self.physical_min == self.logical_min && self.physical_max == self.logical_max;
        if undefined_physical || identity || self.logical_max == self.logical_min {
            return logical as f64;
        }
        let span_l = (self.logical_max - self.logical_min) as f64;
        let span_p = (self.physical_max - self.physical_min) as f64;
        self.physical_min as f64 + (logical - self.logical_min) as f64 * span_p / span_l
    }
}

/// Errors returned by [`ReportDescriptor::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("descriptor truncated at byte {offset}")]
    Truncated { offset: usize },
    #[error("End Collection without matching Collection at byte {offset}")]
    UnbalancedCollection { offset: usize },
    #[error("{open} collection(s) not closed at end of descriptor")]
    UnclosedCollection { open: usize },
    #[error("Pop without matching Push at byte {offset}")]
    StackUnderflow { offset: usize },
    #[error("invalid {what} at byte {offset}")]
    Invalid { what: &'static str, offset: usize },
    #[error("limit exceeded ({what}) at byte {offset}")]
    LimitExceeded { what: &'static str, offset: usize },
}

/// A parsed HID report descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportDescriptor {
    fields: Vec<Field>,
    collections: Vec<Collection>,
    usage_lists: Vec<Vec<Usage>>,
    reports: Vec<ReportLayout>,
    uses_report_ids: bool,
}

#[derive(Debug, Clone, Default)]
struct Globals {
    usage_page: u16,
    logical_min: i64,
    logical_max_signed: i64,
    logical_max_unsigned: i64,
    physical_min: i64,
    physical_max_signed: i64,
    physical_max_unsigned: i64,
    unit_exponent: i8,
    unit: u32,
    report_size: u32,
    report_id: u8,
    report_count: u32,
}

impl Globals {
    /// Many devices encode e.g. a Logical Maximum of 255 in one byte (0xFF),
    /// which would sign-extend to -1. Like Linux and NUT, interpret the
    /// maximum as unsigned whenever the minimum is non-negative.
    fn logical_max(&self) -> i64 {
        if self.logical_min >= 0 {
            self.logical_max_unsigned
        } else {
            self.logical_max_signed
        }
    }

    fn physical_max(&self) -> i64 {
        if self.physical_min >= 0 {
            self.physical_max_unsigned
        } else {
            self.physical_max_signed
        }
    }
}

/// A usage as declared in a local item.
#[derive(Debug, Clone, Copy)]
struct LocalUsage {
    usage: Usage,
    /// Declared as a 4-byte extended usage (page given explicitly).
    explicit_page: bool,
}

#[derive(Debug, Clone, Copy)]
enum LocalEntry {
    Single(LocalUsage),
    Range(LocalUsage, LocalUsage),
}

#[derive(Debug, Default)]
struct Locals {
    entries: Vec<LocalEntry>,
    pending_min: Option<LocalUsage>,
    delimiter_open: bool,
    delimiter_taken: bool,
}

impl Locals {
    /// HID 1.11 §6.2.2.8 / Linux `hid_concatenate_last_usage_page`: a 1- or
    /// 2-byte usage gets the Usage Page in effect at the *Main* item. Walk the
    /// usages backwards and re-page them until one already carries the current
    /// page, so mixed-page local sets keep their earlier pages.
    fn apply_page(&mut self, page: u16) {
        fn repage(u: &mut LocalUsage, page: u16) {
            if !u.explicit_page {
                u.usage.page = page;
            }
        }
        for entry in self.entries.iter_mut().rev() {
            match entry {
                LocalEntry::Single(u) => {
                    if u.explicit_page {
                        continue;
                    }
                    if u.usage.page == page {
                        break;
                    }
                    repage(u, page);
                }
                LocalEntry::Range(a, b) => {
                    if a.explicit_page && b.explicit_page {
                        continue;
                    }
                    let already = (!a.explicit_page && a.usage.page == page)
                        || (!b.explicit_page && b.usage.page == page);
                    if already {
                        break;
                    }
                    repage(a, page);
                    repage(b, page);
                }
            }
        }
    }

    /// Usages in declaration order, ranges expanded, at most `limit` of them.
    fn resolve(&self, limit: usize) -> Vec<Usage> {
        let mut out = Vec::new();
        for entry in &self.entries {
            if out.len() >= limit {
                break;
            }
            match *entry {
                LocalEntry::Single(u) => out.push(u.usage),
                LocalEntry::Range(a, b) => {
                    if a.usage.page != b.usage.page || a.usage.id > b.usage.id {
                        continue;
                    }
                    for id in a.usage.id..=b.usage.id {
                        if out.len() >= limit {
                            break;
                        }
                        out.push(Usage::new(a.usage.page, id));
                    }
                }
            }
        }
        out
    }

    /// Inside a delimiter set only the first usage counts (alternatives are
    /// dropped, as Linux does).
    fn accepts(&mut self) -> bool {
        if !self.delimiter_open {
            return true;
        }
        if self.delimiter_taken {
            return false;
        }
        self.delimiter_taken = true;
        true
    }

    fn push(&mut self, entry: LocalEntry, offset: usize) -> Result<(), ParseError> {
        if self.entries.len() >= MAX_LOCAL_USAGES {
            return Err(ParseError::LimitExceeded {
                what: "usages in one local set",
                offset,
            });
        }
        self.entries.push(entry);
        Ok(())
    }
}

#[allow(clippy::cast_possible_truncation)] // masked to 16 bits
const fn low16(v: u32) -> u16 {
    (v & 0xFFFF) as u16
}

const fn high16(v: u32) -> u16 {
    low16(v >> 16)
}

/// Unit Exponent: the spec encodes a 4-bit two's-complement nibble; some
/// devices send a full signed integer instead. Like Linux, treat any value
/// that fits in a nibble as a nibble, whatever the item size.
fn unit_exponent(unsigned: u32, signed: i64) -> i8 {
    if unsigned & !0xF == 0 {
        let nibble = i8::try_from(unsigned).unwrap_or(0);
        if nibble > 7 { nibble - 16 } else { nibble }
    } else {
        i8::try_from(signed).unwrap_or(0)
    }
}

/// One decoded short item.
#[derive(Debug)]
struct Item {
    tag: u8,
    size: usize,
    unsigned: u32,
    signed: i64,
    offset: usize,
}

/// Parser state while walking the descriptor's items.
#[derive(Debug)]
struct Parser {
    out: ReportDescriptor,
    globals: Globals,
    global_stack: Vec<Globals>,
    locals: Locals,
    open_collections: Vec<usize>,
    array_usages_total: usize,
}

impl Parser {
    fn new() -> Self {
        Self {
            out: ReportDescriptor {
                fields: Vec::new(),
                collections: Vec::new(),
                usage_lists: Vec::new(),
                reports: Vec::new(),
                uses_report_ids: false,
            },
            globals: Globals::default(),
            global_stack: Vec::new(),
            locals: Locals::default(),
            open_collections: Vec::new(),
            array_usages_total: 0,
        }
    }

    fn main_item(&mut self, item: &Item) -> Result<(), ParseError> {
        self.locals.apply_page(self.globals.usage_page);
        let kind = match item.tag {
            0x8 => Some(ReportKind::Input),
            0x9 => Some(ReportKind::Output),
            0xB => Some(ReportKind::Feature),
            _ => None,
        };
        if let Some(kind) = kind {
            self.add_fields(kind, item.unsigned, item.offset)?;
        } else if item.tag == 0xA {
            self.open_collection(item)?;
        } else if item.tag == 0xC {
            self.open_collections
                .pop()
                .ok_or(ParseError::UnbalancedCollection {
                    offset: item.offset,
                })?;
        }
        self.locals = Locals::default();
        Ok(())
    }

    fn open_collection(&mut self, item: &Item) -> Result<(), ParseError> {
        let offset = item.offset;
        if self.open_collections.len() >= MAX_COLLECTION_DEPTH {
            return Err(ParseError::LimitExceeded {
                what: "collection depth",
                offset,
            });
        }
        if self.out.collections.len() >= MAX_COLLECTIONS {
            return Err(ParseError::LimitExceeded {
                what: "collections",
                offset,
            });
        }
        let usage = self
            .locals
            .resolve(1)
            .first()
            .copied()
            .unwrap_or(Usage::new(self.globals.usage_page, 0));
        let mut path = self
            .open_collections
            .last()
            .map(|&parent| self.out.collections[parent].path.clone())
            .unwrap_or_default();
        path.push(usage);
        self.out.collections.push(Collection {
            kind: u8::try_from(item.unsigned).unwrap_or(u8::MAX),
            path,
        });
        self.open_collections.push(self.out.collections.len() - 1);
        Ok(())
    }

    fn global_item(&mut self, item: &Item) -> Result<(), ParseError> {
        let (unsigned, signed, offset) = (item.unsigned, item.signed, item.offset);
        let g = &mut self.globals;
        match item.tag {
            0x0 => g.usage_page = low16(unsigned),
            0x1 => g.logical_min = signed,
            0x2 => {
                g.logical_max_signed = signed;
                g.logical_max_unsigned = i64::from(unsigned);
            }
            0x3 => g.physical_min = signed,
            0x4 => {
                g.physical_max_signed = signed;
                g.physical_max_unsigned = i64::from(unsigned);
            }
            0x5 => g.unit_exponent = unit_exponent(unsigned, signed),
            0x6 => g.unit = unsigned,
            0x7 => g.report_size = unsigned,
            0x8 => {
                g.report_id = match u8::try_from(unsigned) {
                    Ok(id) if id != 0 => id,
                    _ => {
                        return Err(ParseError::Invalid {
                            what: "Report ID",
                            offset,
                        });
                    }
                };
                self.out.uses_report_ids = true;
            }
            0x9 => g.report_count = unsigned,
            0xA => {
                if self.global_stack.len() >= MAX_COLLECTION_DEPTH {
                    return Err(ParseError::LimitExceeded {
                        what: "push depth",
                        offset,
                    });
                }
                self.global_stack.push(self.globals.clone());
            }
            0xB => {
                self.globals = self
                    .global_stack
                    .pop()
                    .ok_or(ParseError::StackUnderflow { offset })?;
            }
            _ => {}
        }
        Ok(())
    }

    fn local_item(&mut self, item: &Item) -> Result<(), ParseError> {
        let explicit_page = item.size == 4;
        let local = LocalUsage {
            usage: if explicit_page {
                Usage::from_extended(item.unsigned)
            } else {
                Usage::new(self.globals.usage_page, low16(item.unsigned))
            },
            explicit_page,
        };
        match item.tag {
            0x0 => {
                if self.locals.accepts() {
                    self.locals.push(LocalEntry::Single(local), item.offset)?;
                }
            }
            0x1 => self.locals.pending_min = Some(local),
            0x2 => {
                if let Some(min) = self.locals.pending_min.take()
                    && self.locals.accepts()
                {
                    self.locals
                        .push(LocalEntry::Range(min, local), item.offset)?;
                }
            }
            0xA => {
                // Delimiter: 1 opens an alternative set, 0 closes it.
                self.locals.delimiter_open = item.unsigned == 1;
                self.locals.delimiter_taken = false;
            }
            _ => {} // designators and strings: not needed
        }
        Ok(())
    }

    fn add_fields(
        &mut self,
        kind: ReportKind,
        flags: u32,
        offset: usize,
    ) -> Result<(), ParseError> {
        let too_large = |what| ParseError::LimitExceeded { what, offset };
        let g = &self.globals;

        if g.report_size > MAX_REPORT_SIZE_BITS {
            return Err(too_large("Report Size"));
        }
        let item_bits = g
            .report_count
            .checked_mul(g.report_size)
            .ok_or(too_large("report length"))?;

        let reports = &mut self.out.reports;
        let layout = if let Some(index) = reports
            .iter()
            .position(|r| r.kind == kind && r.report_id == g.report_id)
        {
            index
        } else {
            reports.push(ReportLayout {
                kind,
                report_id: g.report_id,
                bits: 0,
            });
            reports.len() - 1
        };
        let start = reports[layout].bits;
        let end = start
            .checked_add(item_bits)
            .filter(|bits| *bits <= MAX_REPORT_BITS)
            .ok_or(too_large("report length"))?;

        // A Constant item without usages is padding. A Constant item *with*
        // usages is read-only data: Eaton/MGE declare most UPS values this
        // way. This matches Linux `hid_add_field` and NUT.
        let is_padding = flags & 0x01 != 0 && self.locals.entries.is_empty();
        if !is_padding {
            let count = usize::try_from(g.report_count).map_err(|_| too_large("fields"))?;
            if self.out.fields.len().saturating_add(count) > MAX_FIELDS {
                return Err(too_large("fields"));
            }
            let is_variable = flags & 0x02 != 0;
            let (usages, array_list) = if is_variable {
                (self.locals.resolve(count), None)
            } else {
                let list = self.locals.resolve(MAX_LOCAL_USAGES);
                self.array_usages_total += list.len();
                if self.array_usages_total > MAX_ARRAY_USAGES_TOTAL {
                    return Err(too_large("array usages"));
                }
                let first: Vec<Usage> = list.first().copied().into_iter().collect();
                self.out.usage_lists.push(list);
                (first, Some(self.out.usage_lists.len() - 1))
            };

            // `count * report_size == item_bits` and `start + item_bits == end`,
            // so the slot offsets below stay in range and can't overflow.
            let step =
                usize::try_from(g.report_size.max(1)).map_err(|_| too_large("Report Size"))?;
            let collection = self.open_collections.last().copied();
            for (n, slot_offset) in (0..count).zip((start..end).step_by(step)) {
                let usage = usages
                    .get(n)
                    .or(usages.last())
                    .copied()
                    .unwrap_or(Usage::new(g.usage_page, 0));
                self.out.fields.push(Field {
                    kind,
                    report_id: g.report_id,
                    bit_offset: slot_offset,
                    bit_size: g.report_size,
                    usage,
                    collection,
                    array_usages: array_list,
                    logical_min: g.logical_min,
                    logical_max: g.logical_max(),
                    physical_min: g.physical_min,
                    physical_max: g.physical_max(),
                    unit: g.unit,
                    unit_exponent: g.unit_exponent,
                    flags,
                });
            }
        }

        self.out.reports[layout].bits = end;
        Ok(())
    }

    fn finish(self) -> Result<ReportDescriptor, ParseError> {
        if self.open_collections.is_empty() {
            Ok(self.out)
        } else {
            Err(ParseError::UnclosedCollection {
                open: self.open_collections.len(),
            })
        }
    }
}

impl ReportDescriptor {
    /// Parses a raw HID report descriptor.
    ///
    /// # Errors
    /// Returns [`ParseError`] when the descriptor is truncated, structurally
    /// inconsistent (unbalanced collections or push/pop, report ID 0) or
    /// exceeds one of the module's `MAX_*` limits.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let mut parser = Parser::new();
        let mut pos = 0;
        while pos < bytes.len() {
            let offset = pos;
            let prefix = bytes[pos];

            // Long item: 0xFE, data size, tag, data... (never used by HID 1.11).
            if prefix == 0xFE {
                let size =
                    usize::from(*bytes.get(pos + 1).ok_or(ParseError::Truncated { offset })?);
                pos += 3 + size;
                if pos > bytes.len() {
                    return Err(ParseError::Truncated { offset });
                }
                continue;
            }

            let size = match prefix & 0x03 {
                0 => 0,
                1 => 1,
                2 => 2,
                _ => 4,
            };
            let end = pos + 1 + size;
            let data = bytes
                .get(pos + 1..end)
                .ok_or(ParseError::Truncated { offset })?;
            let item = Item {
                tag: prefix >> 4,
                size,
                unsigned: data
                    .iter()
                    .rev()
                    .fold(0u32, |acc, b| (acc << 8) | u32::from(*b)),
                signed: match *data {
                    [] => 0,
                    [a] => i64::from(i8::from_le_bytes([a])),
                    [a, b] => i64::from(i16::from_le_bytes([a, b])),
                    [a, b, c, d] => i64::from(i32::from_le_bytes([a, b, c, d])),
                    _ => unreachable!("short items carry 0, 1, 2 or 4 bytes"),
                },
                offset,
            };

            match (prefix >> 2) & 0x03 {
                0 => parser.main_item(&item)?,
                1 => parser.global_item(&item)?,
                2 => parser.local_item(&item)?,
                _ => {} // reserved
            }
            pos = end;
        }
        parser.finish()
    }

    /// All fields except padding (constant items without usages), in
    /// descriptor order. Constant fields with usages are read-only data.
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// All collections, in descriptor order.
    #[must_use]
    pub fn collections(&self) -> &[Collection] {
        &self.collections
    }

    /// Bit layout of every report the descriptor declares.
    #[must_use]
    pub fn reports(&self) -> &[ReportLayout] {
        &self.reports
    }

    /// Whether the descriptor declares report IDs.
    #[must_use]
    pub fn uses_report_ids(&self) -> bool {
        self.uses_report_ids
    }

    /// Usages of the collections enclosing `field`, outermost first.
    #[must_use]
    pub fn collection_path(&self, field: &Field) -> &[Usage] {
        field
            .collection
            .and_then(|i| self.collections.get(i))
            .map_or(&[], |c| c.path.as_slice())
    }

    /// The usage list an array field selects from.
    #[must_use]
    pub fn array_usages(&self, field: &Field) -> Option<&[Usage]> {
        field
            .array_usages
            .and_then(|i| self.usage_lists.get(i))
            .map(Vec::as_slice)
    }

    /// Resolves an array field's raw selector value to the selected usage.
    /// `None` means "no usage selected" (or the value is out of range).
    #[must_use]
    pub fn array_selection(&self, field: &Field, raw: i64) -> Option<Usage> {
        let list = self.array_usages(field)?;
        let index = usize::try_from(raw.checked_sub(field.logical_min)?).ok()?;
        list.get(index).copied()
    }

    /// Report IDs that carry at least one data field of `kind`, ascending.
    #[must_use]
    pub fn report_ids(&self, kind: ReportKind) -> Vec<u8> {
        let mut ids: Vec<u8> = self
            .fields
            .iter()
            .filter(|f| f.kind == kind)
            .map(|f| f.report_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Length in bytes of a report's payload (excluding the report ID byte).
    #[must_use]
    pub fn report_len(&self, kind: ReportKind, report_id: u8) -> usize {
        self.reports
            .iter()
            .find(|r| r.kind == kind && r.report_id == report_id)
            .map_or(0, ReportLayout::len_bytes)
    }

    /// Length in bytes of the longest report of `kind`.
    #[must_use]
    pub fn max_report_len(&self, kind: ReportKind) -> usize {
        self.reports
            .iter()
            .filter(|r| r.kind == kind)
            .map(ReportLayout::len_bytes)
            .max()
            .unwrap_or(0)
    }

    /// Fields that live in a given report.
    pub fn fields_in(&self, kind: ReportKind, report_id: u8) -> impl Iterator<Item = &Field> {
        self.fields
            .iter()
            .filter(move |f| f.kind == kind && f.report_id == report_id)
    }
}
