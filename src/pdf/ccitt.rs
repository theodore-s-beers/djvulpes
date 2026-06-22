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
        #[allow(clippy::unreadable_literal)]
        if (-3..=3).contains(&vertical_offset) {
            match vertical_offset {
                0 => writer.write_code(0b1, 1),
                1 => writer.write_code(0b011, 3),
                -1 => writer.write_code(0b010, 3),
                2 => writer.write_code(0b000011, 6),
                -2 => writer.write_code(0b000010, 6),
                3 => writer.write_code(0b0000011, 7),
                -3 => writer.write_code(0b0000010, 7),
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

#[allow(clippy::unreadable_literal)]
const WHITE_TERMINATING_CODES: [CcittCode; 64] = [
    CcittCode {
        bits: 0b00110101,
        len: 8,
    },
    CcittCode {
        bits: 0b000111,
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
        bits: 0b10011,
        len: 5,
    },
    CcittCode {
        bits: 0b10100,
        len: 5,
    },
    CcittCode {
        bits: 0b00111,
        len: 5,
    },
    CcittCode {
        bits: 0b01000,
        len: 5,
    },
    CcittCode {
        bits: 0b001000,
        len: 6,
    },
    CcittCode {
        bits: 0b000011,
        len: 6,
    },
    CcittCode {
        bits: 0b110100,
        len: 6,
    },
    CcittCode {
        bits: 0b110101,
        len: 6,
    },
    CcittCode {
        bits: 0b101010,
        len: 6,
    },
    CcittCode {
        bits: 0b101011,
        len: 6,
    },
    CcittCode {
        bits: 0b0100111,
        len: 7,
    },
    CcittCode {
        bits: 0b0001100,
        len: 7,
    },
    CcittCode {
        bits: 0b0001000,
        len: 7,
    },
    CcittCode {
        bits: 0b0010111,
        len: 7,
    },
    CcittCode {
        bits: 0b0000011,
        len: 7,
    },
    CcittCode {
        bits: 0b0000100,
        len: 7,
    },
    CcittCode {
        bits: 0b0101000,
        len: 7,
    },
    CcittCode {
        bits: 0b0101011,
        len: 7,
    },
    CcittCode {
        bits: 0b0010011,
        len: 7,
    },
    CcittCode {
        bits: 0b0100100,
        len: 7,
    },
    CcittCode {
        bits: 0b0011000,
        len: 7,
    },
    CcittCode {
        bits: 0b00000010,
        len: 8,
    },
    CcittCode {
        bits: 0b00000011,
        len: 8,
    },
    CcittCode {
        bits: 0b00011010,
        len: 8,
    },
    CcittCode {
        bits: 0b00011011,
        len: 8,
    },
    CcittCode {
        bits: 0b00010010,
        len: 8,
    },
    CcittCode {
        bits: 0b00010011,
        len: 8,
    },
    CcittCode {
        bits: 0b00010100,
        len: 8,
    },
    CcittCode {
        bits: 0b00010101,
        len: 8,
    },
    CcittCode {
        bits: 0b00010110,
        len: 8,
    },
    CcittCode {
        bits: 0b00010111,
        len: 8,
    },
    CcittCode {
        bits: 0b00101000,
        len: 8,
    },
    CcittCode {
        bits: 0b00101001,
        len: 8,
    },
    CcittCode {
        bits: 0b00101010,
        len: 8,
    },
    CcittCode {
        bits: 0b00101011,
        len: 8,
    },
    CcittCode {
        bits: 0b00101100,
        len: 8,
    },
    CcittCode {
        bits: 0b00101101,
        len: 8,
    },
    CcittCode {
        bits: 0b00000100,
        len: 8,
    },
    CcittCode {
        bits: 0b00000101,
        len: 8,
    },
    CcittCode {
        bits: 0b00001010,
        len: 8,
    },
    CcittCode {
        bits: 0b00001011,
        len: 8,
    },
    CcittCode {
        bits: 0b01010010,
        len: 8,
    },
    CcittCode {
        bits: 0b01010011,
        len: 8,
    },
    CcittCode {
        bits: 0b01010100,
        len: 8,
    },
    CcittCode {
        bits: 0b01010101,
        len: 8,
    },
    CcittCode {
        bits: 0b00100100,
        len: 8,
    },
    CcittCode {
        bits: 0b00100101,
        len: 8,
    },
    CcittCode {
        bits: 0b01011000,
        len: 8,
    },
    CcittCode {
        bits: 0b01011001,
        len: 8,
    },
    CcittCode {
        bits: 0b01011010,
        len: 8,
    },
    CcittCode {
        bits: 0b01011011,
        len: 8,
    },
    CcittCode {
        bits: 0b01001010,
        len: 8,
    },
    CcittCode {
        bits: 0b01001011,
        len: 8,
    },
    CcittCode {
        bits: 0b00110010,
        len: 8,
    },
    CcittCode {
        bits: 0b00110011,
        len: 8,
    },
    CcittCode {
        bits: 0b00110100,
        len: 8,
    },
];

#[allow(clippy::unreadable_literal)]
const BLACK_TERMINATING_CODES: [CcittCode; 64] = [
    CcittCode {
        bits: 0b0000110111,
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
        bits: 0b00011,
        len: 5,
    },
    CcittCode {
        bits: 0b000101,
        len: 6,
    },
    CcittCode {
        bits: 0b000100,
        len: 6,
    },
    CcittCode {
        bits: 0b0000100,
        len: 7,
    },
    CcittCode {
        bits: 0b0000101,
        len: 7,
    },
    CcittCode {
        bits: 0b0000111,
        len: 7,
    },
    CcittCode {
        bits: 0b00000100,
        len: 8,
    },
    CcittCode {
        bits: 0b00000111,
        len: 8,
    },
    CcittCode {
        bits: 0b000011000,
        len: 9,
    },
    CcittCode {
        bits: 0b0000010111,
        len: 10,
    },
    CcittCode {
        bits: 0b0000011000,
        len: 10,
    },
    CcittCode {
        bits: 0b0000001000,
        len: 10,
    },
    CcittCode {
        bits: 0b00001100111,
        len: 11,
    },
    CcittCode {
        bits: 0b00001101000,
        len: 11,
    },
    CcittCode {
        bits: 0b00001101100,
        len: 11,
    },
    CcittCode {
        bits: 0b00000110111,
        len: 11,
    },
    CcittCode {
        bits: 0b00000101000,
        len: 11,
    },
    CcittCode {
        bits: 0b00000010111,
        len: 11,
    },
    CcittCode {
        bits: 0b00000011000,
        len: 11,
    },
    CcittCode {
        bits: 0b000011001010,
        len: 12,
    },
    CcittCode {
        bits: 0b000011001011,
        len: 12,
    },
    CcittCode {
        bits: 0b000011001100,
        len: 12,
    },
    CcittCode {
        bits: 0b000011001101,
        len: 12,
    },
    CcittCode {
        bits: 0b000001101000,
        len: 12,
    },
    CcittCode {
        bits: 0b000001101001,
        len: 12,
    },
    CcittCode {
        bits: 0b000001101010,
        len: 12,
    },
    CcittCode {
        bits: 0b000001101011,
        len: 12,
    },
    CcittCode {
        bits: 0b000011010010,
        len: 12,
    },
    CcittCode {
        bits: 0b000011010011,
        len: 12,
    },
    CcittCode {
        bits: 0b000011010100,
        len: 12,
    },
    CcittCode {
        bits: 0b000011010101,
        len: 12,
    },
    CcittCode {
        bits: 0b000011010110,
        len: 12,
    },
    CcittCode {
        bits: 0b000011010111,
        len: 12,
    },
    CcittCode {
        bits: 0b000001101100,
        len: 12,
    },
    CcittCode {
        bits: 0b000001101101,
        len: 12,
    },
    CcittCode {
        bits: 0b000011011010,
        len: 12,
    },
    CcittCode {
        bits: 0b000011011011,
        len: 12,
    },
    CcittCode {
        bits: 0b000001010100,
        len: 12,
    },
    CcittCode {
        bits: 0b000001010101,
        len: 12,
    },
    CcittCode {
        bits: 0b000001010110,
        len: 12,
    },
    CcittCode {
        bits: 0b000001010111,
        len: 12,
    },
    CcittCode {
        bits: 0b000001100100,
        len: 12,
    },
    CcittCode {
        bits: 0b000001100101,
        len: 12,
    },
    CcittCode {
        bits: 0b000001010010,
        len: 12,
    },
    CcittCode {
        bits: 0b000001010011,
        len: 12,
    },
    CcittCode {
        bits: 0b000000100100,
        len: 12,
    },
    CcittCode {
        bits: 0b000000110111,
        len: 12,
    },
    CcittCode {
        bits: 0b000000111000,
        len: 12,
    },
    CcittCode {
        bits: 0b000000100111,
        len: 12,
    },
    CcittCode {
        bits: 0b000000101000,
        len: 12,
    },
    CcittCode {
        bits: 0b000001011000,
        len: 12,
    },
    CcittCode {
        bits: 0b000001011001,
        len: 12,
    },
    CcittCode {
        bits: 0b000000101011,
        len: 12,
    },
    CcittCode {
        bits: 0b000000101100,
        len: 12,
    },
    CcittCode {
        bits: 0b000001011010,
        len: 12,
    },
    CcittCode {
        bits: 0b000001100110,
        len: 12,
    },
    CcittCode {
        bits: 0b000001100111,
        len: 12,
    },
];

#[allow(clippy::unreadable_literal)]
const WHITE_MAKEUP_CODES: &[(usize, CcittCode)] = &[
    (
        64,
        CcittCode {
            bits: 0b11011,
            len: 5,
        },
    ),
    (
        128,
        CcittCode {
            bits: 0b10010,
            len: 5,
        },
    ),
    (
        192,
        CcittCode {
            bits: 0b010111,
            len: 6,
        },
    ),
    (
        256,
        CcittCode {
            bits: 0b0110111,
            len: 7,
        },
    ),
    (
        320,
        CcittCode {
            bits: 0b00110110,
            len: 8,
        },
    ),
    (
        384,
        CcittCode {
            bits: 0b00110111,
            len: 8,
        },
    ),
    (
        448,
        CcittCode {
            bits: 0b01100100,
            len: 8,
        },
    ),
    (
        512,
        CcittCode {
            bits: 0b01100101,
            len: 8,
        },
    ),
    (
        576,
        CcittCode {
            bits: 0b01101000,
            len: 8,
        },
    ),
    (
        640,
        CcittCode {
            bits: 0b01100111,
            len: 8,
        },
    ),
    (
        704,
        CcittCode {
            bits: 0b011001100,
            len: 9,
        },
    ),
    (
        768,
        CcittCode {
            bits: 0b011001101,
            len: 9,
        },
    ),
    (
        832,
        CcittCode {
            bits: 0b011010010,
            len: 9,
        },
    ),
    (
        896,
        CcittCode {
            bits: 0b011010011,
            len: 9,
        },
    ),
    (
        960,
        CcittCode {
            bits: 0b011010100,
            len: 9,
        },
    ),
    (
        1024,
        CcittCode {
            bits: 0b011010101,
            len: 9,
        },
    ),
    (
        1088,
        CcittCode {
            bits: 0b011010110,
            len: 9,
        },
    ),
    (
        1152,
        CcittCode {
            bits: 0b011010111,
            len: 9,
        },
    ),
    (
        1216,
        CcittCode {
            bits: 0b011011000,
            len: 9,
        },
    ),
    (
        1280,
        CcittCode {
            bits: 0b011011001,
            len: 9,
        },
    ),
    (
        1344,
        CcittCode {
            bits: 0b011011010,
            len: 9,
        },
    ),
    (
        1408,
        CcittCode {
            bits: 0b011011011,
            len: 9,
        },
    ),
    (
        1472,
        CcittCode {
            bits: 0b010011000,
            len: 9,
        },
    ),
    (
        1536,
        CcittCode {
            bits: 0b010011001,
            len: 9,
        },
    ),
    (
        1600,
        CcittCode {
            bits: 0b010011010,
            len: 9,
        },
    ),
    (
        1664,
        CcittCode {
            bits: 0b011000,
            len: 6,
        },
    ),
    (
        1728,
        CcittCode {
            bits: 0b010011011,
            len: 9,
        },
    ),
    (
        1792,
        CcittCode {
            bits: 0b00000001000,
            len: 11,
        },
    ),
    (
        1856,
        CcittCode {
            bits: 0b00000001100,
            len: 11,
        },
    ),
    (
        1920,
        CcittCode {
            bits: 0b00000001101,
            len: 11,
        },
    ),
    (
        1984,
        CcittCode {
            bits: 0b000000010010,
            len: 12,
        },
    ),
    (
        2048,
        CcittCode {
            bits: 0b000000010011,
            len: 12,
        },
    ),
    (
        2112,
        CcittCode {
            bits: 0b000000010100,
            len: 12,
        },
    ),
    (
        2176,
        CcittCode {
            bits: 0b000000010101,
            len: 12,
        },
    ),
    (
        2240,
        CcittCode {
            bits: 0b000000010110,
            len: 12,
        },
    ),
    (
        2304,
        CcittCode {
            bits: 0b000000010111,
            len: 12,
        },
    ),
    (
        2368,
        CcittCode {
            bits: 0b000000011100,
            len: 12,
        },
    ),
    (
        2432,
        CcittCode {
            bits: 0b000000011101,
            len: 12,
        },
    ),
    (
        2496,
        CcittCode {
            bits: 0b000000011110,
            len: 12,
        },
    ),
    (
        2560,
        CcittCode {
            bits: 0b000000011111,
            len: 12,
        },
    ),
];

#[allow(clippy::unreadable_literal)]
const BLACK_MAKEUP_CODES: &[(usize, CcittCode)] = &[
    (
        64,
        CcittCode {
            bits: 0b0000001111,
            len: 10,
        },
    ),
    (
        128,
        CcittCode {
            bits: 0b000011001000,
            len: 12,
        },
    ),
    (
        192,
        CcittCode {
            bits: 0b000011001001,
            len: 12,
        },
    ),
    (
        256,
        CcittCode {
            bits: 0b000001011011,
            len: 12,
        },
    ),
    (
        320,
        CcittCode {
            bits: 0b000000110011,
            len: 12,
        },
    ),
    (
        384,
        CcittCode {
            bits: 0b000000110100,
            len: 12,
        },
    ),
    (
        448,
        CcittCode {
            bits: 0b000000110101,
            len: 12,
        },
    ),
    (
        512,
        CcittCode {
            bits: 0b0000001101100,
            len: 13,
        },
    ),
    (
        576,
        CcittCode {
            bits: 0b0000001101101,
            len: 13,
        },
    ),
    (
        640,
        CcittCode {
            bits: 0b0000001001010,
            len: 13,
        },
    ),
    (
        704,
        CcittCode {
            bits: 0b0000001001011,
            len: 13,
        },
    ),
    (
        768,
        CcittCode {
            bits: 0b0000001001100,
            len: 13,
        },
    ),
    (
        832,
        CcittCode {
            bits: 0b0000001001101,
            len: 13,
        },
    ),
    (
        896,
        CcittCode {
            bits: 0b0000001110010,
            len: 13,
        },
    ),
    (
        960,
        CcittCode {
            bits: 0b0000001110011,
            len: 13,
        },
    ),
    (
        1024,
        CcittCode {
            bits: 0b0000001110100,
            len: 13,
        },
    ),
    (
        1088,
        CcittCode {
            bits: 0b0000001110101,
            len: 13,
        },
    ),
    (
        1152,
        CcittCode {
            bits: 0b0000001110110,
            len: 13,
        },
    ),
    (
        1216,
        CcittCode {
            bits: 0b0000001110111,
            len: 13,
        },
    ),
    (
        1280,
        CcittCode {
            bits: 0b0000001010010,
            len: 13,
        },
    ),
    (
        1344,
        CcittCode {
            bits: 0b0000001010011,
            len: 13,
        },
    ),
    (
        1408,
        CcittCode {
            bits: 0b0000001010100,
            len: 13,
        },
    ),
    (
        1472,
        CcittCode {
            bits: 0b0000001010101,
            len: 13,
        },
    ),
    (
        1536,
        CcittCode {
            bits: 0b0000001011010,
            len: 13,
        },
    ),
    (
        1600,
        CcittCode {
            bits: 0b0000001011011,
            len: 13,
        },
    ),
    (
        1664,
        CcittCode {
            bits: 0b0000001100100,
            len: 13,
        },
    ),
    (
        1728,
        CcittCode {
            bits: 0b0000001100101,
            len: 13,
        },
    ),
    (
        1792,
        CcittCode {
            bits: 0b00000001000,
            len: 11,
        },
    ),
    (
        1856,
        CcittCode {
            bits: 0b00000001100,
            len: 11,
        },
    ),
    (
        1920,
        CcittCode {
            bits: 0b00000001101,
            len: 11,
        },
    ),
    (
        1984,
        CcittCode {
            bits: 0b000000010010,
            len: 12,
        },
    ),
    (
        2048,
        CcittCode {
            bits: 0b000000010011,
            len: 12,
        },
    ),
    (
        2112,
        CcittCode {
            bits: 0b000000010100,
            len: 12,
        },
    ),
    (
        2176,
        CcittCode {
            bits: 0b000000010101,
            len: 12,
        },
    ),
    (
        2240,
        CcittCode {
            bits: 0b000000010110,
            len: 12,
        },
    ),
    (
        2304,
        CcittCode {
            bits: 0b000000010111,
            len: 12,
        },
    ),
    (
        2368,
        CcittCode {
            bits: 0b000000011100,
            len: 12,
        },
    ),
    (
        2432,
        CcittCode {
            bits: 0b000000011101,
            len: 12,
        },
    ),
    (
        2496,
        CcittCode {
            bits: 0b000000011110,
            len: 12,
        },
    ),
    (
        2560,
        CcittCode {
            bits: 0b000000011111,
            len: 12,
        },
    ),
];
