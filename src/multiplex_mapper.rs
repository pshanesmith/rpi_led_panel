use crate::{error::InvalidVariantError, rgb_matrix::MatrixCreationError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::EnumString, strum::VariantNames)]
#[strum(
    parse_err_fn = InvalidVariantError::new::<Self>,
    parse_err_ty = InvalidVariantError
)]
pub enum MultiplexMapperType {
    Stripe,
    Checkered,
    Spiral,
    ZStripe08,
    ZStripe44,
    ZStripe80,
    Coreman,
    Kaler2Scan,
    P10Z,
    QiangLiQ8,
    InversedZStripe,
    P10Outdoor1R1G1B1,
    P10Outdoor1R1G1B2,
    P10Outdoor1R1G1B3,
    P10Coreman,
    P8Outdoor1R1G1B,
    FlippedStripe,
    P10Outdoor32x16HalfScan,
}

impl MultiplexMapperType {
    pub(crate) fn create(self) -> MultiplexMapper {
        MultiplexMapper {
            kind: self,
            panel_rows: 0,
            panel_cols: 0,
        }
    }

    /// How much wider (and less tall) the panel is electrically than it is visibly.
    const fn stretch_factor(self) -> usize {
        match self {
            Self::Kaler2Scan | Self::P10Z | Self::P10Coreman | Self::P10Outdoor32x16HalfScan => 4,
            _ => 2,
        }
    }
}

/// Maps the visible layout of a panel with an unusual multiplexing scheme to the layout that is electrically
/// driven.
pub(crate) struct MultiplexMapper {
    kind: MultiplexMapperType,
    panel_rows: usize,
    panel_cols: usize,
}

impl MultiplexMapper {
    pub(crate) fn edit_rows_cols(&mut self, rows: &mut usize, cols: &mut usize) {
        self.panel_rows = *rows;
        self.panel_cols = *cols;

        *rows /= self.kind.stretch_factor();
        *cols *= self.kind.stretch_factor();
    }

    pub(crate) fn get_size_mapping(
        &self,
        matrix_width: usize,
        matrix_height: usize,
    ) -> Result<[usize; 2], MatrixCreationError> {
        // Matrix width has been altered. Alter it back.
        let visible_width = matrix_width / self.kind.stretch_factor();
        let visible_height = matrix_height * self.kind.stretch_factor();
        Ok([visible_width, visible_height])
    }

    pub(crate) fn map_visible_to_matrix(
        &self,
        _matrix_width: usize,
        _matrix_height: usize,
        visible_x: usize,
        visible_y: usize,
    ) -> [usize; 2] {
        let stretch_factor = self.kind.stretch_factor();
        let chained_panel = visible_x / self.panel_cols;
        let parallel_panel = visible_y / self.panel_rows;

        let within_panel_x = visible_x % self.panel_cols;
        let within_panel_y = visible_y % self.panel_rows;

        let [new_x, new_y] = self.map_single_panel(within_panel_x, within_panel_y);
        let matrix_x = chained_panel * stretch_factor * self.panel_cols + new_x;
        let matrix_y = parallel_panel * self.panel_rows / stretch_factor + new_y;
        [matrix_x, matrix_y]
    }

    fn map_single_panel(&self, x: usize, y: usize) -> [usize; 2] {
        let rows = self.panel_rows;
        let cols = self.panel_cols;
        match self.kind {
            MultiplexMapperType::Stripe => stripe(rows, cols, x, y, false),
            MultiplexMapperType::FlippedStripe => stripe(rows, cols, x, y, true),
            MultiplexMapperType::Checkered => checkered(rows, cols, x, y),
            MultiplexMapperType::Spiral => spiral(rows, cols, x, y),
            MultiplexMapperType::ZStripe08 => z_stripe(x, y, 0, 8),
            MultiplexMapperType::ZStripe44 => z_stripe(x, y, 4, 4),
            MultiplexMapperType::ZStripe80 => z_stripe(x, y, 8, 0),
            MultiplexMapperType::Coreman => coreman(rows, cols, x, y),
            MultiplexMapperType::Kaler2Scan => kaler_2_scan(x, y),
            MultiplexMapperType::P10Z => p10_z(x, y),
            MultiplexMapperType::QiangLiQ8 => qiang_li_q8(x, y),
            MultiplexMapperType::InversedZStripe => inversed_z_stripe(x, y),
            MultiplexMapperType::P10Outdoor1R1G1B1 => p10_outdoor_1r1g1b_1(x, y),
            MultiplexMapperType::P10Outdoor1R1G1B2 => p10_outdoor_1r1g1b_2(x, y),
            MultiplexMapperType::P10Outdoor1R1G1B3 => p10_outdoor_1r1g1b_3(x, y),
            MultiplexMapperType::P10Coreman => p10_coreman(x, y),
            MultiplexMapperType::P8Outdoor1R1G1B => p8_outdoor_1r1g1b(x, y),
            MultiplexMapperType::P10Outdoor32x16HalfScan => p10_outdoor_32x16_half_scan(x, y),
        }
    }
}

/// The row within the stretched panel for the common "two stripes per half" layouts.
fn stripe_row(rows: usize, y: usize) -> usize {
    (y / (rows / 2)) * (rows / 4) + y % (rows / 4)
}

fn stripe(rows: usize, cols: usize, x: usize, y: usize, flipped: bool) -> [usize; 2] {
    let is_top_stripe = ((y % (rows / 2)) < rows / 4) != flipped;
    let matrix_x = if is_top_stripe { x + cols } else { x };
    [matrix_x, stripe_row(rows, y)]
}

fn checkered(rows: usize, cols: usize, x: usize, y: usize) -> [usize; 2] {
    let is_top_check = (y % (rows / 2)) < rows / 4;
    let is_left_check = x < cols / 2;
    let matrix_x = if is_top_check {
        if is_left_check {
            x + cols / 2
        } else {
            x + cols
        }
    } else if is_left_check {
        x
    } else {
        x + cols / 2
    };
    [matrix_x, stripe_row(rows, y)]
}

fn spiral(rows: usize, cols: usize, x: usize, y: usize) -> [usize; 2] {
    let is_top_stripe = (y % (rows / 2)) < rows / 4;
    let panel_quarter = cols / 4;
    let quarter = x / panel_quarter;
    let offset = x % panel_quarter;
    let matrix_x = (2 * quarter * panel_quarter)
        + if is_top_stripe {
            panel_quarter - 1 - offset
        } else {
            panel_quarter + offset
        };
    [matrix_x, stripe_row(rows, y)]
}

fn z_stripe(x: usize, y: usize, even_vblock_offset: usize, odd_vblock_offset: usize) -> [usize; 2] {
    let tile_width = 8;
    let tile_height = 4;

    let vert_block_is_odd = (y / tile_height) % 2;

    let even_vblock_shift = (1 - vert_block_is_odd) * even_vblock_offset;
    let odd_vblock_shift = vert_block_is_odd * odd_vblock_offset;

    let matrix_x = x + ((x + even_vblock_shift) / tile_width) * tile_width + odd_vblock_shift;
    let matrix_y = (y % tile_height) + tile_height * (y / (tile_height * 2));
    [matrix_x, matrix_y]
}

fn coreman(rows: usize, cols: usize, x: usize, y: usize) -> [usize; 2] {
    let is_left_check = x < cols / 2;

    if (y <= 7) || (16..=23).contains(&y) {
        let matrix_x = ((x / (cols / 2)) * cols) + (x % (cols / 2));
        let matrix_y = if (y & (rows / 4)) == 0 {
            stripe_row(rows, y)
        } else {
            0
        };
        [matrix_x, matrix_y]
    } else {
        let matrix_x = if is_left_check {
            x + cols / 2
        } else {
            x + cols
        };
        [matrix_x, stripe_row(rows, y)]
    }
}

fn kaler_2_scan(x: usize, y: usize) -> [usize; 2] {
    // Now we have a 128x4 matrix
    let offset: isize = if ((y % 4) / 2) == 0 { -1 } else { 1 }; // Add or subtract
    let delta_offset: isize = if offset < 0 { 7 } else { 8 };
    let delta_column: isize = if ((y % 8) / 4) == 0 { 64 } else { 0 };

    let matrix_y = y % 2 + (y / 8) * 2;
    let matrix_x =
        (delta_column + (16 * (x as isize / 8)) + delta_offset + ((x as isize % 8) * offset))
            as usize;
    [matrix_x, matrix_y]
}

fn p10_z(x: usize, y: usize) -> [usize; 2] {
    let y_comp = match y {
        0 | 1 | 8 | 9 => 127,
        2 | 3 | 10 | 11 => 112,
        4 | 5 | 12 | 13 => 111,
        6 | 7 | 14 | 15 => 96,
        _ => 0,
    };

    let matrix_x = match y {
        0 | 1 | 4 | 5 | 8 | 9 | 12 | 13 => y_comp - x - 24 * (x / 8),
        _ => y_comp + x - 40 * (x / 8),
    };

    let matrix_y = match y {
        0 | 2 | 4 | 6 => 3,
        1 | 3 | 5 | 7 => 2,
        8 | 10 | 12 | 14 => 1,
        9 | 11 | 13 | 15 => 0,
        _ => y,
    };

    [matrix_x, matrix_y]
}

fn qiang_li_q8(x: usize, y: usize) -> [usize; 2] {
    let matrix_x = if (15..=19).contains(&y) || (5..=9).contains(&y) {
        x + (4 * (x / 4))
    } else {
        x + (4 + 4 * (x / 4))
    };
    let matrix_y = y % 5 + (y / 10) * 5;
    [matrix_x, matrix_y]
}

fn inversed_z_stripe(x: usize, y: usize) -> [usize; 2] {
    let tile_width = 8;
    let tile_height = 4;

    let vert_block_is_even = (y / tile_height).is_multiple_of(2);
    let even_offset: [usize; 8] = [15, 13, 11, 9, 7, 5, 3, 1];

    let matrix_x = x
        + (x / tile_width) * tile_width
        + if vert_block_is_even {
            even_offset[x % 8]
        } else {
            0
        };
    let matrix_y = (y % tile_height) + tile_height * (y / (tile_height * 2));
    [matrix_x, matrix_y]
}

const P10_TILE_WIDTH: usize = 8;
const P10_TILE_HEIGHT: usize = 4;
const P10_EVEN_VBLOCK_OFFSET: usize = 0;
const P10_ODD_VBLOCK_OFFSET: usize = 8;

fn p10_row(y: usize) -> usize {
    (y % P10_TILE_HEIGHT) + P10_TILE_HEIGHT * (y / (P10_TILE_HEIGHT * 2))
}

fn p10_outdoor_1r1g1b_1(x: usize, y: usize) -> [usize; 2] {
    let vblock_is_even = (y / P10_TILE_HEIGHT).is_multiple_of(2);

    let matrix_x = P10_TILE_WIDTH * (1 + usize::from(vblock_is_even) + 2 * (x / P10_TILE_WIDTH))
        - (x % P10_TILE_WIDTH)
        - 1;
    [matrix_x, p10_row(y)]
}

fn p10_outdoor_1r1g1b_2(x: usize, y: usize) -> [usize; 2] {
    let vblock_is_even = (y / P10_TILE_HEIGHT).is_multiple_of(2);
    let even_vblock_shift = usize::from(vblock_is_even) * P10_EVEN_VBLOCK_OFFSET;
    let odd_vblock_shift = usize::from(!vblock_is_even) * P10_ODD_VBLOCK_OFFSET;

    let matrix_x = if vblock_is_even {
        P10_TILE_WIDTH * (1 + 2 * (x / P10_TILE_WIDTH)) - (x % P10_TILE_WIDTH) - 1
    } else {
        x + ((x + even_vblock_shift) / P10_TILE_WIDTH) * P10_TILE_WIDTH + odd_vblock_shift
    };
    [matrix_x, p10_row(y)]
}

fn p10_outdoor_1r1g1b_3(x: usize, y: usize) -> [usize; 2] {
    let vblock_is_even = (y / P10_TILE_HEIGHT).is_multiple_of(2);
    let even_vblock_shift = usize::from(vblock_is_even) * P10_EVEN_VBLOCK_OFFSET;
    let odd_vblock_shift = usize::from(!vblock_is_even) * P10_ODD_VBLOCK_OFFSET;

    let matrix_x = if vblock_is_even {
        x + ((x + even_vblock_shift) / P10_TILE_WIDTH) * P10_TILE_WIDTH + odd_vblock_shift
    } else {
        P10_TILE_WIDTH * (2 + 2 * (x / P10_TILE_WIDTH)) - (x % P10_TILE_WIDTH) - 1
    };
    [matrix_x, p10_row(y)]
}

fn p10_coreman(x: usize, y: usize) -> [usize; 2] {
    // Row offset 8,8,8,8,0,0,0,0,8,8,8,8,0,0,0,0
    let mut mul_y = if (y & 4) > 0 { 0 } else { 8 };

    // Row offset 9,9,8,8,1,1,0,0,9,9,8,8,1,1,0,0
    mul_y += usize::from((y & 2) == 0);
    mul_y += (x >> 2) & !1; // Drop lsb

    let matrix_x = (mul_y << 3) + x % 8;
    let matrix_y = (y & 1) + ((y >> 2) & !1);
    [matrix_x, matrix_y]
}

fn p10_outdoor_32x16_half_scan(x: usize, y: usize) -> [usize; 2] {
    let base = (x / 8) * 32;
    let reverse = (y % 4) / 2 == 0;
    let offset = (3 - ((y % 8) / 2)) * 8;
    let dx = x % 8;

    let matrix_y = if y / 8 == 0 {
        if y.is_multiple_of(2) { 0 } else { 1 }
    } else if y.is_multiple_of(2) {
        2
    } else {
        3
    };
    let matrix_x = base
        + if reverse {
            offset + (7 - dx)
        } else {
            offset + dx
        };
    [matrix_x, matrix_y]
}

const P8_TILE_WIDTH: usize = 8;
const P8_TILE_HEIGHT: usize = 5;

fn p8_outdoor_1r1g1b(x: usize, y: usize) -> [usize; 2] {
    let vblock_is_even = (y / P8_TILE_HEIGHT).is_multiple_of(2);
    let matrix_x = if vblock_is_even {
        P8_TILE_WIDTH * (1 + P8_TILE_WIDTH - 2 * (x / P8_TILE_WIDTH)) + P8_TILE_WIDTH
            - (x % P8_TILE_WIDTH)
            - 1
    } else {
        P8_TILE_WIDTH * (1 + P8_TILE_WIDTH - 2 * (x / P8_TILE_WIDTH)) - P8_TILE_WIDTH
            + (x % P8_TILE_WIDTH)
    };

    let matrix_y =
        (P8_TILE_HEIGHT - y % P8_TILE_HEIGHT) + P8_TILE_HEIGHT * (1 - y / (P8_TILE_HEIGHT * 2)) - 1;
    [matrix_x, matrix_y]
}
