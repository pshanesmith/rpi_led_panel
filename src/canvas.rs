use std::sync::Arc;

use crate::{
    RGBMatrixConfig,
    color::{CIE1931_LOOKUP, ColorLookup},
    config::K_BIT_PLANES,
    error::InvalidVariantError,
    hardware_mapping::{GpioBits, HardwareMapping},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Color {
    Red,
    Green,
    Blue,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, strum::EnumString, strum::VariantNames,
)]
#[strum(serialize_all = "UPPERCASE")]
#[strum(
    parse_err_fn = InvalidVariantError::new::<Self>,
    parse_err_ty = InvalidVariantError
)]
pub enum LedSequence {
    #[default]
    Rgb,
    Rbg,
    Grb,
    Gbr,
    Brg,
    Bgr,
}

impl LedSequence {
    /// The order in which the colors appear on the panel's R, G and B inputs.
    const fn colors(self) -> [Color; 3] {
        match self {
            LedSequence::Rgb => [Color::Red, Color::Green, Color::Blue],
            LedSequence::Rbg => [Color::Red, Color::Blue, Color::Green],
            LedSequence::Grb => [Color::Green, Color::Red, Color::Blue],
            LedSequence::Gbr => [Color::Green, Color::Blue, Color::Red],
            LedSequence::Brg => [Color::Blue, Color::Red, Color::Green],
            LedSequence::Bgr => [Color::Blue, Color::Green, Color::Red],
        }
    }

    /// Returns the GPIO bits that drive `color`: the input at the position of `color` in the sequence.
    fn get_gpio(
        self,
        color: Color,
        red_bits: GpioBits,
        green_bits: GpioBits,
        blue_bits: GpioBits,
    ) -> GpioBits {
        let inputs = [red_bits, green_bits, blue_bits];
        let position = self
            .colors()
            .iter()
            .position(|c| *c == color)
            .expect("Every sequence contains every color.");
        inputs[position]
    }
}

/// Marks a pixel that is not connected to any LED.
const UNUSED_PIXEL: u32 = u32::MAX;

#[derive(Clone, Copy)]
pub(crate) struct PixelDesignator {
    /// Offset into the bit plane buffer, or [`UNUSED_PIXEL`].
    gpio_word: u32,
    r_bit: GpioBits,
    g_bit: GpioBits,
    b_bit: GpioBits,
    mask: GpioBits,
}

impl PixelDesignator {
    pub(crate) fn new(hardware_mapping: &HardwareMapping, sequence: LedSequence) -> Self {
        let h = hardware_mapping;
        let r = h.panels.red_bits();
        let g = h.panels.green_bits();
        let b = h.panels.blue_bits();
        Self {
            gpio_word: UNUSED_PIXEL,
            r_bit: sequence.get_gpio(Color::Red, r, g, b),
            g_bit: sequence.get_gpio(Color::Green, r, g, b),
            b_bit: sequence.get_gpio(Color::Blue, r, g, b),
            mask: !0,
        }
    }

    /// Whether the pixel is connected to an LED.
    pub(crate) fn is_used(&self) -> bool {
        self.gpio_word != UNUSED_PIXEL
    }
}

#[derive(Clone)]
pub(crate) struct PixelDesignatorMap {
    width: usize,
    height: usize,
    pixel_designator: PixelDesignator,
    buffer: Vec<PixelDesignator>,
    /// The bits of LEDs that no visible pixel maps to, by their offset in the first bit plane. They stay off when
    /// filling the canvas.
    hidden_bits: Vec<(usize, GpioBits)>,
}

impl PixelDesignatorMap {
    /// Creates the map for the physical matrix of `config.parallel` chains of `config.chain_length` panels.
    pub(crate) fn new(pixel_designator: PixelDesignator, config: &RGBMatrixConfig) -> Self {
        let width = config.cols * config.chain_length;
        let height = config.rows * config.parallel;
        let mut map = Self::blank(pixel_designator, width, height);
        let hm = config.hardware_mapping;
        let double_rows = config.double_rows();
        for y in 0..height {
            for x in 0..width {
                let position = y * width + x;
                let d = &mut map.buffer[position];
                let offset = (y % double_rows) * (width * K_BIT_PLANES) + x;
                d.gpio_word = u32::try_from(offset).expect("Matrix is too large.");

                let panel = y / config.rows;
                let color_bits = hm.panels.color_bits[panel];
                let (r, g, b) = if y - panel * config.rows < double_rows {
                    (color_bits.r1, color_bits.g1, color_bits.b1)
                } else {
                    (color_bits.r2, color_bits.g2, color_bits.b2)
                };

                d.r_bit = config.led_sequence.get_gpio(Color::Red, r, g, b);
                d.g_bit = config.led_sequence.get_gpio(Color::Green, r, g, b);
                d.b_bit = config.led_sequence.get_gpio(Color::Blue, r, g, b);
                d.mask = !(d.r_bit | d.g_bit | d.b_bit);
            }
        }
        map
    }

    /// Creates a map with every pixel unused, to be filled by a pixel mapper.
    pub(crate) fn blank(pixel_designator: PixelDesignator, width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixel_designator,
            buffer: vec![pixel_designator; width * height],
            hidden_bits: Vec::new(),
        }
    }

    /// Finds the LEDs of the physical `matrix` that this map doesn't reach.
    pub(crate) fn find_hidden_bits(&mut self, matrix: &PixelDesignatorMap) {
        let used = |d: &&PixelDesignator| d.is_used();
        let len = matrix
            .buffer
            .iter()
            .filter(used)
            .map(|d| d.gpio_word as usize + 1)
            .max()
            .unwrap_or(0);
        let mut bits: Vec<GpioBits> = vec![0; len];
        for d in matrix.buffer.iter().filter(used) {
            bits[d.gpio_word as usize] |= !d.mask;
        }
        for d in self.buffer.iter().filter(used) {
            bits[d.gpio_word as usize] &= d.mask;
        }
        self.hidden_bits = bits
            .into_iter()
            .enumerate()
            .filter(|(_, bits)| *bits != 0)
            .collect();
    }

    fn position(&self, x: usize, y: usize) -> Option<usize> {
        (x < self.width && y < self.height).then_some(y * self.width + x)
    }

    pub(crate) fn get(&self, x: usize, y: usize) -> Option<&PixelDesignator> {
        self.position(x, y).map(|position| &self.buffer[position])
    }

    pub(crate) fn get_mut(&mut self, x: usize, y: usize) -> Option<&mut PixelDesignator> {
        self.position(x, y)
            .map(|position| &mut self.buffer[position])
    }

    fn get_pixel_designator(&self) -> PixelDesignator {
        self.pixel_designator
    }

    pub(crate) fn width(&self) -> usize {
        self.width
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }
}

/// A frame for the matrix. Draw to it and pass it to [`crate::RGBMatrix::update_on_vsync`] to show it.
#[derive(Clone)]
pub struct Canvas {
    cols: usize,
    double_rows: usize,
    bitplane_buffer: Vec<GpioBits>,
    shared_mapper: Arc<PixelDesignatorMap>,
    pwm_bits: usize,
    brightness: u8,
    luminance_correct: bool,
    inverse_colors: bool,
    color_lookup: &'static ColorLookup,
    interlaced: bool,
}

impl Canvas {
    pub(crate) fn new(config: &RGBMatrixConfig, shared_mapper: Arc<PixelDesignatorMap>) -> Self {
        let color_lookup = &*CIE1931_LOOKUP;
        let cols = config.cols * config.chain_length;
        let double_rows = config.double_rows();
        let mut canvas = Self {
            cols,
            double_rows,
            bitplane_buffer: vec![0; double_rows * cols * K_BIT_PLANES],
            shared_mapper,
            pwm_bits: config.pwm_bits.clamp(1, K_BIT_PLANES),
            brightness: config.led_brightness.clamp(1, 100),
            luminance_correct: true,
            inverse_colors: config.inverse_colors,
            color_lookup,
            interlaced: config.interlaced,
        };
        if canvas.inverse_colors {
            // An empty buffer would light up all LEDs.
            canvas.fill(0, 0, 0);
        }
        canvas
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.shared_mapper.height
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.shared_mapper.width
    }

    /// The number of rows that are multiplexed, each of which shows one row of both sub-panels.
    pub(crate) fn double_rows(&self) -> usize {
        self.double_rows
    }

    /// The number of columns of the whole chain.
    pub(crate) fn columns(&self) -> usize {
        self.cols
    }

    /// The double row that is shown at the given position of the scan.
    pub(crate) fn display_row(&self, row_loop: usize) -> usize {
        if !self.interlaced {
            return row_loop;
        }
        // Round up so that an odd number of rows still shows every row exactly once.
        let half_double = self.double_rows.div_ceil(2);
        if row_loop < half_double {
            2 * row_loop
        } else {
            2 * (row_loop - half_double) + 1
        }
    }

    /// The lowest bit plane to show. Depending on if we do dithering, we might not always show the lowest bits.
    pub(crate) fn start_bit(&self, pwm_low_bit: usize) -> usize {
        (K_BIT_PLANES - self.pwm_bits).max(pwm_low_bit)
    }

    fn position_at(&self, double_row: usize, column: usize, bit: usize) -> usize {
        double_row * (self.cols * K_BIT_PLANES) + bit * self.cols + column
    }

    /// The GPIO words of one bit plane of a double row, one per column.
    pub(crate) fn row_at(&self, double_row: usize, column: usize, bit: usize) -> &[GpioBits] {
        let start = self.position_at(double_row, column, bit);
        &self.bitplane_buffer[start..start + self.cols]
    }

    fn row_at_mut(&mut self, double_row: usize, column: usize, bit: usize) -> &mut [GpioBits] {
        let start = self.position_at(double_row, column, bit);
        &mut self.bitplane_buffer[start..start + self.cols]
    }

    fn map_color(&self, r: u8, g: u8, b: u8) -> [u16; 3] {
        self.color_lookup.map_rgb(
            self.brightness,
            self.luminance_correct,
            self.inverse_colors,
            [r, g, b],
        )
    }

    /// Writes the mapped color to the bit planes of a pixel.
    fn write_pixel(&mut self, designator: PixelDesignator, [red, green, blue]: [u16; 3]) {
        let PixelDesignator {
            gpio_word,
            r_bit,
            g_bit,
            b_bit,
            mask: designator_mask,
        } = designator;

        if !designator.is_used() {
            return;
        }
        let pos_start = gpio_word as usize;

        let min_bit_plane = K_BIT_PLANES - self.pwm_bits;

        (min_bit_plane..K_BIT_PLANES).for_each(|plane| {
            let pos = pos_start + self.cols * plane;
            let mask = 1 << plane;
            let mut color_bits = 0;
            if (red & mask) != 0 {
                color_bits |= r_bit;
            };
            if (green & mask) != 0 {
                color_bits |= g_bit;
            };
            if (blue & mask) != 0 {
                color_bits |= b_bit;
            };
            self.bitplane_buffer[pos] &= designator_mask;
            self.bitplane_buffer[pos] |= color_bits;
        });
    }

    pub fn set_pixel(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8) {
        let Some(&designator) = self.shared_mapper.get(x, y) else {
            return;
        };
        let color = self.map_color(r, g, b);
        self.write_pixel(designator, color);
    }

    /// Sets the pixels of the `width` x `height` rectangle at `x`, `y` to the `[r, g, b]` colors, row by row.
    /// Pixels outside of the canvas are skipped.
    pub fn set_pixels(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        colors: &[[u8; 3]],
    ) {
        if width == 0 {
            return;
        }
        for (i, &[r, g, b]) in colors.iter().take(width * height).enumerate() {
            self.set_pixel(x + i % width, y + i / width, r, g, b);
        }
    }

    /// Fills the `width` x `height` rectangle at `x`, `y` with one color. The part outside of the canvas is
    /// skipped.
    #[allow(clippy::too_many_arguments)]
    pub fn sub_fill(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        r: u8,
        g: u8,
        b: u8,
    ) {
        let color = self.map_color(r, g, b);
        let x_end = x.saturating_add(width).min(self.width());
        let y_end = y.saturating_add(height).min(self.height());
        let mapper = Arc::clone(&self.shared_mapper);
        for row in y..y_end {
            for col in x..x_end {
                if let Some(&designator) = mapper.get(col, row) {
                    self.write_pixel(designator, color);
                }
            }
        }
    }

    /// The GPIO bits of one bit plane for the mapped color, for all pixels.
    fn plane_bits(&self, [red, green, blue]: [u16; 3], plane: usize) -> GpioBits {
        let PixelDesignator {
            r_bit,
            g_bit,
            b_bit,
            ..
        } = self.shared_mapper.get_pixel_designator();
        let mask = 1 << plane;
        let mut plane_bits = 0;
        if (red & mask) == mask {
            plane_bits |= r_bit;
        };
        if (green & mask) == mask {
            plane_bits |= g_bit;
        };
        if (blue & mask) == mask {
            plane_bits |= b_bit;
        };
        plane_bits
    }

    pub fn fill(&mut self, r: u8, g: u8, b: u8) {
        let color = self.map_color(r, g, b);
        let black = self.map_color(0, 0, 0);

        (K_BIT_PLANES - self.pwm_bits..K_BIT_PLANES).for_each(|plane| {
            let plane_bits = self.plane_bits(color, plane);
            (0..self.double_rows).for_each(|row| {
                self.row_at_mut(row, 0, plane).fill(plane_bits);
            });
            // LEDs that aren't part of the canvas stay off.
            let off_bits = self.plane_bits(black, plane);
            if plane_bits != off_bits {
                for &(offset, bits) in &self.shared_mapper.hidden_bits {
                    let word = &mut self.bitplane_buffer[offset + self.cols * plane];
                    *word = (*word & !bits) | (off_bits & bits);
                }
            }
        });
    }

    /// Set PWM bits used for output, between 1 and 11. Default is 11, but if you only deal with simple
    /// comic-colors, 1 might be sufficient. Lower values require less CPU.
    ///
    /// The setting carries over to the canvases returned by [`crate::RGBMatrix::update_on_vsync`].
    pub fn set_pwm_bits(&mut self, pwm_bits: usize) {
        let pwm_bits = pwm_bits.clamp(1, K_BIT_PLANES);
        // Planes that become visible may still hold data from long ago, so turn them off.
        let black = self.map_color(0, 0, 0);
        for plane in K_BIT_PLANES - pwm_bits..K_BIT_PLANES - self.pwm_bits.min(pwm_bits) {
            let off_bits = self.plane_bits(black, plane);
            for row in 0..self.double_rows {
                self.row_at_mut(row, 0, plane).fill(off_bits);
            }
        }
        self.pwm_bits = pwm_bits;
    }

    #[must_use]
    pub fn pwm_bits(&self) -> usize {
        self.pwm_bits
    }

    /// Set the canvas' brightness in percent. Only affects pixels drawn afterwards.
    ///
    /// The setting carries over to the canvases returned by [`crate::RGBMatrix::update_on_vsync`].
    pub fn set_brightness(&mut self, brightness: u8) {
        self.brightness = brightness.clamp(1, 100);
    }

    #[must_use]
    pub fn brightness(&self) -> u8 {
        self.brightness
    }

    /// Turn the CIE1931 luminance correction on or off. It is on by default and makes the brightness steps
    /// look even to the eye. Only affects pixels drawn afterwards.
    ///
    /// The setting carries over to the canvases returned by [`crate::RGBMatrix::update_on_vsync`].
    pub fn set_luminance_correct(&mut self, on: bool) {
        self.luminance_correct = on;
    }

    #[must_use]
    pub fn luminance_correct(&self) -> bool {
        self.luminance_correct
    }

    /// The raw content of the canvas, e.g. to store a frame and show it again later with
    /// [`Canvas::deserialize`]. It is only valid for canvases of matrices with the same configuration.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        self.bitplane_buffer
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect()
    }

    /// Restores the content from [`Canvas::serialize`]. Returns `false` and leaves the canvas unchanged if the
    /// data has the wrong size.
    pub fn deserialize(&mut self, data: &[u8]) -> bool {
        const WORD_BYTES: usize = size_of::<GpioBits>();
        if data.len() != self.bitplane_buffer.len() * WORD_BYTES {
            return false;
        }
        let (words, _) = data.as_chunks::<WORD_BYTES>();
        for (word, bytes) in self.bitplane_buffer.iter_mut().zip(words) {
            *word = GpioBits::from_le_bytes(*bytes);
        }
        true
    }

    /// Copies the content of another canvas of the same matrix.
    ///
    /// # Panics
    ///
    /// If the canvases have different sizes.
    pub fn copy_from(&mut self, other: &Canvas) {
        self.bitplane_buffer.copy_from_slice(&other.bitplane_buffer);
    }
}

#[cfg(feature = "drawing")]
pub mod embedded_graphics_support {
    use super::Canvas;
    use embedded_graphics::{
        Pixel,
        draw_target::DrawTarget,
        pixelcolor::Rgb888,
        prelude::{Dimensions, OriginDimensions, RgbColor, Size},
        primitives::Rectangle,
    };

    impl OriginDimensions for Canvas {
        fn size(&self) -> Size {
            Size::new(self.width() as u32, self.height() as u32)
        }
    }

    impl DrawTarget for Canvas {
        type Color = Rgb888;

        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(coord, color) in pixels {
                // `DrawTarget` implementation are required to discard any out of bounds pixels without returning
                // an error or causing a panic.
                if (0..self.width() as i32).contains(&coord.x)
                    && (0..self.height() as i32).contains(&coord.y)
                {
                    self.set_pixel(
                        coord.x as usize,
                        coord.y as usize,
                        color.r(),
                        color.g(),
                        color.b(),
                    );
                }
            }
            Ok(())
        }

        fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
            // Only the part of the area on the canvas is drawn.
            let area = area.intersection(&self.bounding_box());
            let (Some(x), Some(y)) = (
                usize::try_from(area.top_left.x).ok(),
                usize::try_from(area.top_left.y).ok(),
            ) else {
                return Ok(());
            };
            self.sub_fill(
                x,
                y,
                area.size.width as usize,
                area.size.height as usize,
                color.r(),
                color.g(),
                color.b(),
            );
            Ok(())
        }

        fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
            self.fill(color.r(), color.g(), color.b());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Canvas, Color, LedSequence, PixelDesignator, PixelDesignatorMap};
    use crate::{RGBMatrixConfig, config::K_BIT_PLANES};
    use std::sync::Arc;

    #[test]
    fn led_sequence_matches_upstream() {
        // Upstream drives each color on the input at the color's position in the sequence.
        let (r, g, b) = (1, 2, 4);
        let bits = |seq: LedSequence| {
            [Color::Red, Color::Green, Color::Blue].map(|color| seq.get_gpio(color, r, g, b))
        };
        assert_eq!(bits(LedSequence::Rgb), [r, g, b]);
        assert_eq!(bits(LedSequence::Gbr), [b, r, g]);
        assert_eq!(bits(LedSequence::Brg), [g, b, r]);
        assert_eq!(bits(LedSequence::Bgr), [b, g, r]);
    }

    fn canvas(config: &RGBMatrixConfig) -> Canvas {
        let designator = PixelDesignator::new(&config.hardware_mapping, config.led_sequence);
        Canvas::new(
            config,
            Arc::new(PixelDesignatorMap::new(designator, config)),
        )
    }

    #[test]
    fn increasing_pwm_bits_clears_stale_planes() {
        let mut canvas = canvas(&RGBMatrixConfig::default());
        canvas.fill(255, 255, 255);
        canvas.set_pwm_bits(1);
        canvas.set_pwm_bits(K_BIT_PLANES);
        let low_plane = canvas.row_at(0, 0, 0);
        assert!(low_plane.iter().all(|word| *word == 0));
    }

    #[test]
    fn fill_keeps_hidden_leds_off() {
        // The second panel is left out of the canvas.
        let mut config = RGBMatrixConfig {
            rows: 16,
            cols: 32,
            chain_length: 2,
            pixelmapper: vec!["Remap:32,16|0,0n|0,0x".parse().unwrap()],
            ..Default::default()
        };
        let map = crate::RGBMatrix::create_pixel_map(&mut config).unwrap();
        let mut canvas = Canvas::new(&config, Arc::new(map));
        canvas.fill(255, 255, 255);
        // Only the bits of the first chain are sent to the panels.
        let chain_bits = config.hardware_mapping.panels.color_bits[0].used_bits();
        // The panels are counted from the Raspberry Pi, so the left out one is on the left of the matrix.
        for plane in 0..K_BIT_PLANES {
            let row = canvas.row_at(0, 0, plane);
            assert!(row[..32].iter().all(|word| word & chain_bits == 0));
            assert!(row[32..].iter().all(|word| word & chain_bits == chain_bits));
        }
    }

    #[test]
    fn inverse_colors_keep_black_leds_off() {
        let config = RGBMatrixConfig {
            inverse_colors: true,
            ..Default::default()
        };
        let mut canvas = canvas(&config);
        let red = config.hardware_mapping.panels.color_bits[0].r1;
        // Black is a high signal on all color lines, from the start.
        assert!(canvas.row_at(0, 0, 10).iter().all(|word| word & red == red));
        canvas.set_pixel(0, 0, 255, 0, 0);
        assert_eq!(canvas.row_at(0, 0, 10)[0] & red, 0);
    }

    #[test]
    fn sub_fill_and_set_pixels_match_set_pixel() {
        let config = RGBMatrixConfig::default();
        let mut expected = canvas(&config);
        for (x, y) in [(2, 3), (3, 3), (2, 4), (3, 4)] {
            expected.set_pixel(x, y, 10, 20, 30);
        }
        let mut filled = canvas(&config);
        filled.sub_fill(2, 3, 2, 2, 10, 20, 30);
        assert_eq!(filled.serialize(), expected.serialize());
        let mut set = canvas(&config);
        set.set_pixels(2, 3, 2, 2, &[[10, 20, 30]; 4]);
        assert_eq!(set.serialize(), expected.serialize());
        // Clipped at the edge of the canvas.
        let mut clipped = canvas(&config);
        clipped.sub_fill(62, 62, 10, 10, 255, 255, 255);
        clipped.set_pixels(62, 0, 4, 1, &[[255, 255, 255]; 4]);
    }

    #[test]
    fn serializes_and_copies() {
        let config = RGBMatrixConfig::default();
        let mut original = canvas(&config);
        original.set_pixel(5, 6, 1, 2, 3);
        let data = original.serialize();
        let mut restored = canvas(&config);
        assert!(restored.deserialize(&data));
        assert_eq!(restored.serialize(), data);
        assert!(!restored.deserialize(&data[1..]));
        let mut copy = canvas(&config);
        copy.copy_from(&original);
        assert_eq!(copy.serialize(), data);
    }

    #[test]
    fn luminance_correction_can_be_turned_off() {
        let config = RGBMatrixConfig::default();
        let mut canvas = canvas(&config);
        let red = config.hardware_mapping.panels.color_bits[0].r1;
        // 128 is 1024 without correction, which only sets the highest bit plane.
        canvas.set_luminance_correct(false);
        canvas.set_pixel(0, 0, 128, 0, 0);
        assert_eq!(canvas.row_at(0, 0, 10)[0] & red, red);
        assert_eq!(canvas.row_at(0, 0, 9)[0] & red, 0);
        // With correction, it is much darker.
        canvas.set_luminance_correct(true);
        canvas.set_pixel(0, 0, 128, 0, 0);
        assert_eq!(canvas.row_at(0, 0, 10)[0] & red, 0);
    }

    #[test]
    fn pwm_bits_are_clamped() {
        let mut canvas = canvas(&RGBMatrixConfig::default());
        canvas.set_pwm_bits(20);
        assert_eq!(canvas.pwm_bits(), K_BIT_PLANES);
        canvas.set_pwm_bits(0);
        assert_eq!(canvas.pwm_bits(), 1);
    }
}
