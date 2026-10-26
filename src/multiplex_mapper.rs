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
    P10Outdoor32x16QuarterScan,
    P3Outdoor64x64,
    DoubleZ,
    P4Outdoor80x40,
    /// [`MultiplexMapperType::ZStripe44`] for 40x20 panels with 1:5 scan.
    ZStripe44For40x20,
    /// [`MultiplexMapperType::P10Outdoor1R1G1B2`] for 40x20 panels with 1:5 scan.
    P10Outdoor1R1G1B2For40x20,
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
            Self::Kaler2Scan
            | Self::P10Z
            | Self::P10Coreman
            | Self::P10Outdoor32x16HalfScan
            | Self::P10Outdoor32x16QuarterScan => 4,
            _ => 2,
        }
    }

    /// How much less tall the panel is electrically than it is visibly. Usually the same as the stretch factor.
    const fn row_divisor(self) -> usize {
        match self {
            // Has half the stretch factor in y compared to x.
            Self::P10Outdoor32x16QuarterScan => 2,
            _ => self.stretch_factor(),
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

        *rows /= self.kind.row_divisor();
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
            MultiplexMapperType::ZStripe08 => z_stripe(x, y, 0, 8, 4),
            MultiplexMapperType::ZStripe44 => z_stripe(x, y, 4, 4, 4),
            MultiplexMapperType::ZStripe80 => z_stripe(x, y, 8, 0, 4),
            MultiplexMapperType::ZStripe44For40x20 => z_stripe(x, y, 4, 4, 5),
            MultiplexMapperType::Coreman => coreman(rows, cols, x, y),
            MultiplexMapperType::Kaler2Scan => kaler_2_scan(x, y),
            MultiplexMapperType::P10Z => p10_z(x, y),
            MultiplexMapperType::QiangLiQ8 => qiang_li_q8(x, y),
            MultiplexMapperType::InversedZStripe => inversed_z_stripe(x, y),
            MultiplexMapperType::P10Outdoor1R1G1B1 => p10_outdoor_1r1g1b_1(x, y),
            MultiplexMapperType::P10Outdoor1R1G1B2 => p10_outdoor_1r1g1b_2(x, y, P10_TILE_HEIGHT),
            MultiplexMapperType::P10Outdoor1R1G1B2For40x20 => p10_outdoor_1r1g1b_2(x, y, 5),
            MultiplexMapperType::P10Outdoor1R1G1B3 => p10_outdoor_1r1g1b_3(x, y),
            MultiplexMapperType::P10Coreman => p10_coreman(x, y),
            MultiplexMapperType::P8Outdoor1R1G1B => p8_outdoor_1r1g1b(x, y),
            MultiplexMapperType::P10Outdoor32x16HalfScan => p10_outdoor_32x16_half_scan(x, y),
            MultiplexMapperType::P10Outdoor32x16QuarterScan => p10_outdoor_32x16_quarter_scan(x, y),
            MultiplexMapperType::P3Outdoor64x64 => p3_outdoor_64x64(rows, x, y),
            MultiplexMapperType::DoubleZ => double_z(rows, cols, x, y),
            MultiplexMapperType::P4Outdoor80x40 => p4_outdoor_80x40(x, y),
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

/// `tile_height` is 4 for the common 1:4 scan panels; 1:5 scan panels (e.g. 40x20 outdoor modules) use 5.
fn z_stripe(
    x: usize,
    y: usize,
    even_vblock_offset: usize,
    odd_vblock_offset: usize,
    tile_height: usize,
) -> [usize; 2] {
    let tile_width = 8;

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

fn p10_row(y: usize, tile_height: usize) -> usize {
    (y % tile_height) + tile_height * (y / (tile_height * 2))
}

fn p10_outdoor_1r1g1b_1(x: usize, y: usize) -> [usize; 2] {
    let vblock_is_even = (y / P10_TILE_HEIGHT).is_multiple_of(2);

    let matrix_x = P10_TILE_WIDTH * (1 + usize::from(vblock_is_even) + 2 * (x / P10_TILE_WIDTH))
        - (x % P10_TILE_WIDTH)
        - 1;
    [matrix_x, p10_row(y, P10_TILE_HEIGHT)]
}

/// `tile_height` is 4 for the common 16x16 1:4 scan modules; 1:5 scan modules (e.g. 40x20) use 5.
fn p10_outdoor_1r1g1b_2(x: usize, y: usize, tile_height: usize) -> [usize; 2] {
    let vblock_is_even = (y / tile_height).is_multiple_of(2);
    let even_vblock_shift = usize::from(vblock_is_even) * P10_EVEN_VBLOCK_OFFSET;
    let odd_vblock_shift = usize::from(!vblock_is_even) * P10_ODD_VBLOCK_OFFSET;

    let matrix_x = if vblock_is_even {
        P10_TILE_WIDTH * (1 + 2 * (x / P10_TILE_WIDTH)) - (x % P10_TILE_WIDTH) - 1
    } else {
        x + ((x + even_vblock_shift) / P10_TILE_WIDTH) * P10_TILE_WIDTH + odd_vblock_shift
    };
    [matrix_x, p10_row(y, tile_height)]
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
    [matrix_x, p10_row(y, P10_TILE_HEIGHT)]
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

/// P10 quarter scan panel, e.g. <https://www.ebay.com.au/itm/175517677191>
fn p10_outdoor_32x16_quarter_scan(x: usize, y: usize) -> [usize; 2] {
    let cell_starting_point = (x / 8) * 32;
    let delta_x = x % 8;
    let offset = (3 - (y / 4)) * 8;
    [cell_starting_point + delta_x + offset, y % 4]
}

/// P3 RGB panel 64x64 with the pattern
/// ```text
/// [1] [3]
///  | \ |
/// [0] [2]
/// ```
fn p3_outdoor_64x64(rows: usize, x: usize, y: usize) -> [usize; 2] {
    let is_top_stripe = (y % (rows / 2)) < rows / 4;
    let matrix_x = (x * 2) + usize::from(is_top_stripe);
    [matrix_x, stripe_row(rows, y)]
}

fn double_z(rows: usize, cols: usize, x: usize, y: usize) -> [usize; 2] {
    let quarter_rows = rows / 4;
    let quarter_cols = cols / 4;

    let y_quarter = y / quarter_rows; // 0..3
    let x_quarter = x / quarter_cols; // 0..3

    let offset_y = y % quarter_rows;
    let mut offset_x = x % quarter_cols;

    let flip_quarter = y_quarter == 1 || y_quarter == 3;
    if flip_quarter {
        offset_x = quarter_cols - 1 - offset_x;
    }

    let is_top_stripe = (y % (rows / 2)) >= quarter_rows;

    // Mirrors within half panels depending on stripe position.
    let base_x = 2 * x_quarter * quarter_cols;
    let top_offset = if is_top_stripe {
        quarter_cols - 1 - offset_x
    } else {
        quarter_cols + offset_x
    };

    [
        base_x + top_offset,
        (y_quarter / 2) * quarter_rows + offset_y,
    ]
}

/// P4 outdoor panel with 80x40 pixels.
fn p4_outdoor_80x40(x: usize, y: usize) -> [usize; 2] {
    let tile_width = 8;
    let tile_height = 10;
    let vblock_is_odd = !(y / tile_height).is_multiple_of(2);
    let hblock = x / tile_width;

    let matrix_x = if vblock_is_odd {
        (x % tile_width) + (2 * tile_width * hblock) + tile_width
    } else {
        // Even tiles have reverse x-order.
        tile_width - 1 - (x % tile_width) + (2 * tile_width * hblock)
    };
    let matrix_y = (y % tile_height) + tile_height * (y / (tile_height * 2));
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

#[cfg(test)]
mod tests {
    use super::MultiplexMapperType;

    #[test]
    fn quarter_scan_has_half_the_vertical_stretch() {
        let mut mapper = MultiplexMapperType::P10Outdoor32x16QuarterScan.create();
        let (mut rows, mut cols) = (16, 32);
        mapper.edit_rows_cols(&mut rows, &mut cols);
        assert_eq!((rows, cols), (8, 128));
        assert_eq!(mapper.map_visible_to_matrix(128, 8, 0, 0), [24, 0]);
        assert_eq!(mapper.map_visible_to_matrix(128, 8, 9, 5), [49, 1]);
    }

    #[test]
    fn p4_outdoor_80x40_reverses_even_tiles() {
        let mut mapper = MultiplexMapperType::P4Outdoor80x40.create();
        let (mut rows, mut cols) = (40, 80);
        mapper.edit_rows_cols(&mut rows, &mut cols);
        assert_eq!(mapper.map_visible_to_matrix(160, 20, 0, 0), [7, 0]);
        assert_eq!(mapper.map_visible_to_matrix(160, 20, 0, 10), [8, 0]);
        assert_eq!(mapper.map_visible_to_matrix(160, 20, 9, 21), [22, 11]);
    }
}
