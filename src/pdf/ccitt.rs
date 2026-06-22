pub(super) fn group4_encode(width: u32, height: u32, bytes: &[u8]) -> Vec<u8> {
    let width = usize::try_from(width).expect("validated bitonal width should fit usize");
    let height = usize::try_from(height).expect("validated bitonal height should fit usize");
    let row_bytes = width.div_ceil(8);
    let mut writer = CcittBitWriter::default();
    let mut reference_changes = Vec::new();

    for row_index in 0..height {
        let row_start = row_index
            .checked_mul(row_bytes)
            .expect("validated bitonal row offset should not overflow");
        let row = &bytes[row_start..row_start + row_bytes];
        let current_changes = ccitt_row_changes(row, width);
        ccitt_group4_encode_row(width, &current_changes, &reference_changes, &mut writer);
        reference_changes = current_changes;
    }

    writer.write_code(0b0000_0000_0001, 12);
    writer.write_code(0b0000_0000_0001, 12);
    writer.finish()
}

fn ccitt_row_changes(row: &[u8], width: usize) -> Vec<usize> {
    let mut changes = Vec::new();
    let mut previous = false;

    for x in 0..width {
        let current = row[x / 8] & (0x80 >> (x % 8)) != 0;
        if current != previous {
            changes.push(x);
            previous = current;
        }
    }

    changes
}

fn ccitt_group4_encode_row(
    width: usize,
    current_changes: &[usize],
    reference_changes: &[usize],
    writer: &mut CcittBitWriter,
) {
    let mut a0 = CcittPosition::BeforeLine;
    let mut color = false;

    while a0.run_start() < width {
        let a1 = ccitt_next_change(current_changes, a0.search_start(), width);
        let b1 = ccitt_next_reference_change_to_color(
            reference_changes,
            a0.search_start(),
            !color,
            width,
        );
        let b2 = ccitt_next_reference_change_to_color(
            reference_changes,
            b1.saturating_add(1),
            color,
            width,
        );

        if b2 < a1 {
            writer.write_code(0b0001, 4);
            a0 = CcittPosition::At(b2);
            continue;
        }

        let vertical_offset = isize::try_from(a1).expect("CCITT a1 should fit isize")
            - isize::try_from(b1).expect("CCITT b1 should fit isize");
        if (-3..=3).contains(&vertical_offset) {
            match vertical_offset {
                0 => writer.write_code(0b1, 1),
                1 => writer.write_code(0b011, 3),
                -1 => writer.write_code(0b010, 3),
                2 => writer.write_code(0b00_0011, 6),
                -2 => writer.write_code(0b00_0010, 6),
                3 => writer.write_code(0b000_0011, 7),
                -3 => writer.write_code(0b000_0010, 7),
                _ => unreachable!("vertical offset range already checked"),
            }
            a0 = CcittPosition::At(a1);
            color = !color;
            continue;
        }

        let a2 = ccitt_next_change(current_changes, a1.saturating_add(1), width);
        writer.write_code(0b001, 3);
        let first_run = a1 - a0.run_start();
        let second_run = a2 - a1;
        ccitt_write_run(writer, first_run, color);
        ccitt_write_run(writer, second_run, !color);
        a0 = CcittPosition::At(a2);
    }
}

fn ccitt_next_change(changes: &[usize], start: usize, width: usize) -> usize {
    let index = changes.partition_point(|change| *change < start);
    changes.get(index).copied().unwrap_or(width)
}

fn ccitt_next_reference_change_to_color(
    changes: &[usize],
    start: usize,
    target_color: bool,
    width: usize,
) -> usize {
    let mut index = changes.partition_point(|change| *change < start);
    while let Some(&change) = changes.get(index) {
        let color_after_change = index.is_multiple_of(2);
        if color_after_change == target_color {
            return change;
        }
        index += 1;
    }

    width
}

fn ccitt_write_run(writer: &mut CcittBitWriter, mut run: usize, black: bool) {
    while run >= 2624 {
        let code = ccitt_makeup_code(2560, black);
        writer.write_code(code.bits, code.len);
        run -= 2560;
    }
    if run >= 64 {
        let makeup = (run / 64) * 64;
        let code = ccitt_makeup_code(makeup, black);
        writer.write_code(code.bits, code.len);
        run -= makeup;
    }

    let code = ccitt_terminating_code(run, black);
    writer.write_code(code.bits, code.len);
}

const fn ccitt_terminating_code(run: usize, black: bool) -> CcittCode {
    if black {
        BLACK_TERMINATING_CODES[run]
    } else {
        WHITE_TERMINATING_CODES[run]
    }
}

fn ccitt_makeup_code(run: usize, black: bool) -> CcittCode {
    let table = if black {
        BLACK_MAKEUP_CODES
    } else {
        WHITE_MAKEUP_CODES
    };
    table
        .iter()
        .find_map(|(candidate, code)| (*candidate == run).then_some(*code))
        .expect("CCITT makeup run should have a code")
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct CcittCode {
    bits: u16,
    len: u8,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum CcittPosition {
    BeforeLine,
    At(usize),
}

impl CcittPosition {
    const fn run_start(self) -> usize {
        match self {
            Self::BeforeLine => 0,
            Self::At(index) => index,
        }
    }

    const fn search_start(self) -> usize {
        match self {
            Self::BeforeLine => 0,
            Self::At(index) => index.saturating_add(1),
        }
    }
}

#[derive(Debug, Default)]
struct CcittBitWriter {
    bytes: Vec<u8>,
    current: u8,
    used_bits: u8,
}

impl CcittBitWriter {
    fn write_code(&mut self, bits: u16, len: u8) {
        for shift in (0..len).rev() {
            let bit = u8::from((bits >> shift) & 1 != 0);
            self.current |= bit << (7 - self.used_bits);
            self.used_bits += 1;
            if self.used_bits == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.used_bits = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used_bits != 0 {
            self.bytes.push(self.current);
        }

        self.bytes
    }
}
const WHITE_TERMINATING_CODES: [CcittCode; 64] = [
    CcittCode {
        bits: 0b0011_0101,
        len: 8,
    },
    CcittCode {
        bits: 0b00_0111,
        len: 6,
    },
    CcittCode {
        bits: 0b0111,
        len: 4,
    },
    CcittCode {
        bits: 0b1000,
        len: 4,
    },
    CcittCode {
        bits: 0b1011,
        len: 4,
    },
    CcittCode {
        bits: 0b1100,
        len: 4,
    },
    CcittCode {
        bits: 0b1110,
        len: 4,
    },
    CcittCode {
        bits: 0b1111,
        len: 4,
    },
    CcittCode {
        bits: 0b1_0011,
        len: 5,
    },
    CcittCode {
        bits: 0b1_0100,
        len: 5,
    },
    CcittCode {
        bits: 0b0_0111,
        len: 5,
    },
    CcittCode {
        bits: 0b0_1000,
        len: 5,
    },
    CcittCode {
        bits: 0b00_1000,
        len: 6,
    },
    CcittCode {
        bits: 0b00_0011,
        len: 6,
    },
    CcittCode {
        bits: 0b11_0100,
        len: 6,
    },
    CcittCode {
        bits: 0b11_0101,
        len: 6,
    },
    CcittCode {
        bits: 0b10_1010,
        len: 6,
    },
    CcittCode {
        bits: 0b10_1011,
        len: 6,
    },
    CcittCode {
        bits: 0b010_0111,
        len: 7,
    },
    CcittCode {
        bits: 0b000_1100,
        len: 7,
    },
    CcittCode {
        bits: 0b000_1000,
        len: 7,
    },
    CcittCode {
        bits: 0b001_0111,
        len: 7,
    },
    CcittCode {
        bits: 0b000_0011,
        len: 7,
    },
    CcittCode {
        bits: 0b000_0100,
        len: 7,
    },
    CcittCode {
        bits: 0b010_1000,
        len: 7,
    },
    CcittCode {
        bits: 0b010_1011,
        len: 7,
    },
    CcittCode {
        bits: 0b001_0011,
        len: 7,
    },
    CcittCode {
        bits: 0b010_0100,
        len: 7,
    },
    CcittCode {
        bits: 0b001_1000,
        len: 7,
    },
    CcittCode {
        bits: 0b0000_0010,
        len: 8,
    },
    CcittCode {
        bits: 0b0000_0011,
        len: 8,
    },
    CcittCode {
        bits: 0b0001_1010,
        len: 8,
    },
    CcittCode {
        bits: 0b0001_1011,
        len: 8,
    },
    CcittCode {
        bits: 0b0001_0010,
        len: 8,
    },
    CcittCode {
        bits: 0b0001_0011,
        len: 8,
    },
    CcittCode {
        bits: 0b0001_0100,
        len: 8,
    },
    CcittCode {
        bits: 0b0001_0101,
        len: 8,
    },
    CcittCode {
        bits: 0b0001_0110,
        len: 8,
    },
    CcittCode {
        bits: 0b0001_0111,
        len: 8,
    },
    CcittCode {
        bits: 0b0010_1000,
        len: 8,
    },
    CcittCode {
        bits: 0b0010_1001,
        len: 8,
    },
    CcittCode {
        bits: 0b0010_1010,
        len: 8,
    },
    CcittCode {
        bits: 0b0010_1011,
        len: 8,
    },
    CcittCode {
        bits: 0b0010_1100,
        len: 8,
    },
    CcittCode {
        bits: 0b0010_1101,
        len: 8,
    },
    CcittCode {
        bits: 0b0000_0100,
        len: 8,
    },
    CcittCode {
        bits: 0b0000_0101,
        len: 8,
    },
    CcittCode {
        bits: 0b0000_1010,
        len: 8,
    },
    CcittCode {
        bits: 0b0000_1011,
        len: 8,
    },
    CcittCode {
        bits: 0b0101_0010,
        len: 8,
    },
    CcittCode {
        bits: 0b0101_0011,
        len: 8,
    },
    CcittCode {
        bits: 0b0101_0100,
        len: 8,
    },
    CcittCode {
        bits: 0b0101_0101,
        len: 8,
    },
    CcittCode {
        bits: 0b0010_0100,
        len: 8,
    },
    CcittCode {
        bits: 0b0010_0101,
        len: 8,
    },
    CcittCode {
        bits: 0b0101_1000,
        len: 8,
    },
    CcittCode {
        bits: 0b0101_1001,
        len: 8,
    },
    CcittCode {
        bits: 0b0101_1010,
        len: 8,
    },
    CcittCode {
        bits: 0b0101_1011,
        len: 8,
    },
    CcittCode {
        bits: 0b0100_1010,
        len: 8,
    },
    CcittCode {
        bits: 0b0100_1011,
        len: 8,
    },
    CcittCode {
        bits: 0b0011_0010,
        len: 8,
    },
    CcittCode {
        bits: 0b0011_0011,
        len: 8,
    },
    CcittCode {
        bits: 0b0011_0100,
        len: 8,
    },
];
const BLACK_TERMINATING_CODES: [CcittCode; 64] = [
    CcittCode {
        bits: 0b00_0011_0111,
        len: 10,
    },
    CcittCode {
        bits: 0b010,
        len: 3,
    },
    CcittCode { bits: 0b11, len: 2 },
    CcittCode { bits: 0b10, len: 2 },
    CcittCode {
        bits: 0b011,
        len: 3,
    },
    CcittCode {
        bits: 0b0011,
        len: 4,
    },
    CcittCode {
        bits: 0b0010,
        len: 4,
    },
    CcittCode {
        bits: 0b0_0011,
        len: 5,
    },
    CcittCode {
        bits: 0b00_0101,
        len: 6,
    },
    CcittCode {
        bits: 0b00_0100,
        len: 6,
    },
    CcittCode {
        bits: 0b000_0100,
        len: 7,
    },
    CcittCode {
        bits: 0b000_0101,
        len: 7,
    },
    CcittCode {
        bits: 0b000_0111,
        len: 7,
    },
    CcittCode {
        bits: 0b0000_0100,
        len: 8,
    },
    CcittCode {
        bits: 0b0000_0111,
        len: 8,
    },
    CcittCode {
        bits: 0b0_0001_1000,
        len: 9,
    },
    CcittCode {
        bits: 0b00_0001_0111,
        len: 10,
    },
    CcittCode {
        bits: 0b00_0001_1000,
        len: 10,
    },
    CcittCode {
        bits: 0b00_0000_1000,
        len: 10,
    },
    CcittCode {
        bits: 0b000_0110_0111,
        len: 11,
    },
    CcittCode {
        bits: 0b000_0110_1000,
        len: 11,
    },
    CcittCode {
        bits: 0b000_0110_1100,
        len: 11,
    },
    CcittCode {
        bits: 0b000_0011_0111,
        len: 11,
    },
    CcittCode {
        bits: 0b000_0010_1000,
        len: 11,
    },
    CcittCode {
        bits: 0b000_0001_0111,
        len: 11,
    },
    CcittCode {
        bits: 0b000_0001_1000,
        len: 11,
    },
    CcittCode {
        bits: 0b0000_1100_1010,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_1100_1011,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_1100_1100,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_1100_1101,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0110_1000,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0110_1001,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0110_1010,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0110_1011,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_1101_0010,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_1101_0011,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_1101_0100,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_1101_0101,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_1101_0110,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_1101_0111,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0110_1100,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0110_1101,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_1101_1010,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_1101_1011,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0101_0100,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0101_0101,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0101_0110,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0101_0111,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0110_0100,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0110_0101,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0101_0010,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0101_0011,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0010_0100,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0011_0111,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0011_1000,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0010_0111,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0010_1000,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0101_1000,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0101_1001,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0010_1011,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0010_1100,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0101_1010,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0110_0110,
        len: 12,
    },
    CcittCode {
        bits: 0b0000_0110_0111,
        len: 12,
    },
];
const WHITE_MAKEUP_CODES: &[(usize, CcittCode)] = &[
    (
        64,
        CcittCode {
            bits: 0b1_1011,
            len: 5,
        },
    ),
    (
        128,
        CcittCode {
            bits: 0b1_0010,
            len: 5,
        },
    ),
    (
        192,
        CcittCode {
            bits: 0b01_0111,
            len: 6,
        },
    ),
    (
        256,
        CcittCode {
            bits: 0b011_0111,
            len: 7,
        },
    ),
    (
        320,
        CcittCode {
            bits: 0b0011_0110,
            len: 8,
        },
    ),
    (
        384,
        CcittCode {
            bits: 0b0011_0111,
            len: 8,
        },
    ),
    (
        448,
        CcittCode {
            bits: 0b0110_0100,
            len: 8,
        },
    ),
    (
        512,
        CcittCode {
            bits: 0b0110_0101,
            len: 8,
        },
    ),
    (
        576,
        CcittCode {
            bits: 0b0110_1000,
            len: 8,
        },
    ),
    (
        640,
        CcittCode {
            bits: 0b0110_0111,
            len: 8,
        },
    ),
    (
        704,
        CcittCode {
            bits: 0b0_1100_1100,
            len: 9,
        },
    ),
    (
        768,
        CcittCode {
            bits: 0b0_1100_1101,
            len: 9,
        },
    ),
    (
        832,
        CcittCode {
            bits: 0b0_1101_0010,
            len: 9,
        },
    ),
    (
        896,
        CcittCode {
            bits: 0b0_1101_0011,
            len: 9,
        },
    ),
    (
        960,
        CcittCode {
            bits: 0b0_1101_0100,
            len: 9,
        },
    ),
    (
        1024,
        CcittCode {
            bits: 0b0_1101_0101,
            len: 9,
        },
    ),
    (
        1088,
        CcittCode {
            bits: 0b0_1101_0110,
            len: 9,
        },
    ),
    (
        1152,
        CcittCode {
            bits: 0b0_1101_0111,
            len: 9,
        },
    ),
    (
        1216,
        CcittCode {
            bits: 0b0_1101_1000,
            len: 9,
        },
    ),
    (
        1280,
        CcittCode {
            bits: 0b0_1101_1001,
            len: 9,
        },
    ),
    (
        1344,
        CcittCode {
            bits: 0b0_1101_1010,
            len: 9,
        },
    ),
    (
        1408,
        CcittCode {
            bits: 0b0_1101_1011,
            len: 9,
        },
    ),
    (
        1472,
        CcittCode {
            bits: 0b0_1001_1000,
            len: 9,
        },
    ),
    (
        1536,
        CcittCode {
            bits: 0b0_1001_1001,
            len: 9,
        },
    ),
    (
        1600,
        CcittCode {
            bits: 0b0_1001_1010,
            len: 9,
        },
    ),
    (
        1664,
        CcittCode {
            bits: 0b01_1000,
            len: 6,
        },
    ),
    (
        1728,
        CcittCode {
            bits: 0b0_1001_1011,
            len: 9,
        },
    ),
    (
        1792,
        CcittCode {
            bits: 0b000_0000_1000,
            len: 11,
        },
    ),
    (
        1856,
        CcittCode {
            bits: 0b000_0000_1100,
            len: 11,
        },
    ),
    (
        1920,
        CcittCode {
            bits: 0b000_0000_1101,
            len: 11,
        },
    ),
    (
        1984,
        CcittCode {
            bits: 0b0000_0001_0010,
            len: 12,
        },
    ),
    (
        2048,
        CcittCode {
            bits: 0b0000_0001_0011,
            len: 12,
        },
    ),
    (
        2112,
        CcittCode {
            bits: 0b0000_0001_0100,
            len: 12,
        },
    ),
    (
        2176,
        CcittCode {
            bits: 0b0000_0001_0101,
            len: 12,
        },
    ),
    (
        2240,
        CcittCode {
            bits: 0b0000_0001_0110,
            len: 12,
        },
    ),
    (
        2304,
        CcittCode {
            bits: 0b0000_0001_0111,
            len: 12,
        },
    ),
    (
        2368,
        CcittCode {
            bits: 0b0000_0001_1100,
            len: 12,
        },
    ),
    (
        2432,
        CcittCode {
            bits: 0b0000_0001_1101,
            len: 12,
        },
    ),
    (
        2496,
        CcittCode {
            bits: 0b0000_0001_1110,
            len: 12,
        },
    ),
    (
        2560,
        CcittCode {
            bits: 0b0000_0001_1111,
            len: 12,
        },
    ),
];
const BLACK_MAKEUP_CODES: &[(usize, CcittCode)] = &[
    (
        64,
        CcittCode {
            bits: 0b00_0000_1111,
            len: 10,
        },
    ),
    (
        128,
        CcittCode {
            bits: 0b0000_1100_1000,
            len: 12,
        },
    ),
    (
        192,
        CcittCode {
            bits: 0b0000_1100_1001,
            len: 12,
        },
    ),
    (
        256,
        CcittCode {
            bits: 0b0000_0101_1011,
            len: 12,
        },
    ),
    (
        320,
        CcittCode {
            bits: 0b0000_0011_0011,
            len: 12,
        },
    ),
    (
        384,
        CcittCode {
            bits: 0b0000_0011_0100,
            len: 12,
        },
    ),
    (
        448,
        CcittCode {
            bits: 0b0000_0011_0101,
            len: 12,
        },
    ),
    (
        512,
        CcittCode {
            bits: 0b0_0000_0110_1100,
            len: 13,
        },
    ),
    (
        576,
        CcittCode {
            bits: 0b0_0000_0110_1101,
            len: 13,
        },
    ),
    (
        640,
        CcittCode {
            bits: 0b0_0000_0100_1010,
            len: 13,
        },
    ),
    (
        704,
        CcittCode {
            bits: 0b0_0000_0100_1011,
            len: 13,
        },
    ),
    (
        768,
        CcittCode {
            bits: 0b0_0000_0100_1100,
            len: 13,
        },
    ),
    (
        832,
        CcittCode {
            bits: 0b0_0000_0100_1101,
            len: 13,
        },
    ),
    (
        896,
        CcittCode {
            bits: 0b0_0000_0111_0010,
            len: 13,
        },
    ),
    (
        960,
        CcittCode {
            bits: 0b0_0000_0111_0011,
            len: 13,
        },
    ),
    (
        1024,
        CcittCode {
            bits: 0b0_0000_0111_0100,
            len: 13,
        },
    ),
    (
        1088,
        CcittCode {
            bits: 0b0_0000_0111_0101,
            len: 13,
        },
    ),
    (
        1152,
        CcittCode {
            bits: 0b0_0000_0111_0110,
            len: 13,
        },
    ),
    (
        1216,
        CcittCode {
            bits: 0b0_0000_0111_0111,
            len: 13,
        },
    ),
    (
        1280,
        CcittCode {
            bits: 0b0_0000_0101_0010,
            len: 13,
        },
    ),
    (
        1344,
        CcittCode {
            bits: 0b0_0000_0101_0011,
            len: 13,
        },
    ),
    (
        1408,
        CcittCode {
            bits: 0b0_0000_0101_0100,
            len: 13,
        },
    ),
    (
        1472,
        CcittCode {
            bits: 0b0_0000_0101_0101,
            len: 13,
        },
    ),
    (
        1536,
        CcittCode {
            bits: 0b0_0000_0101_1010,
            len: 13,
        },
    ),
    (
        1600,
        CcittCode {
            bits: 0b0_0000_0101_1011,
            len: 13,
        },
    ),
    (
        1664,
        CcittCode {
            bits: 0b0_0000_0110_0100,
            len: 13,
        },
    ),
    (
        1728,
        CcittCode {
            bits: 0b0_0000_0110_0101,
            len: 13,
        },
    ),
    (
        1792,
        CcittCode {
            bits: 0b000_0000_1000,
            len: 11,
        },
    ),
    (
        1856,
        CcittCode {
            bits: 0b000_0000_1100,
            len: 11,
        },
    ),
    (
        1920,
        CcittCode {
            bits: 0b000_0000_1101,
            len: 11,
        },
    ),
    (
        1984,
        CcittCode {
            bits: 0b0000_0001_0010,
            len: 12,
        },
    ),
    (
        2048,
        CcittCode {
            bits: 0b0000_0001_0011,
            len: 12,
        },
    ),
    (
        2112,
        CcittCode {
            bits: 0b0000_0001_0100,
            len: 12,
        },
    ),
    (
        2176,
        CcittCode {
            bits: 0b0000_0001_0101,
            len: 12,
        },
    ),
    (
        2240,
        CcittCode {
            bits: 0b0000_0001_0110,
            len: 12,
        },
    ),
    (
        2304,
        CcittCode {
            bits: 0b0000_0001_0111,
            len: 12,
        },
    ),
    (
        2368,
        CcittCode {
            bits: 0b0000_0001_1100,
            len: 12,
        },
    ),
    (
        2432,
        CcittCode {
            bits: 0b0000_0001_1101,
            len: 12,
        },
    ),
    (
        2496,
        CcittCode {
            bits: 0b0000_0001_1110,
            len: 12,
        },
    ),
    (
        2560,
        CcittCode {
            bits: 0b0000_0001_1111,
            len: 12,
        },
    ),
];
