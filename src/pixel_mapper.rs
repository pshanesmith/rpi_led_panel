use crate::{
    multiplex_mapper::MultiplexMapper,
    named_pixel_mapper::{MappingDirection, NamedPixelMapper},
    rgb_matrix::MatrixCreationError,
};

/// A pixel mapper is a way for you to map pixels of LED matrixes to a different
/// layout. If you have an implementation of a [`PixelMapper`], you can give it
/// to the `RGBMatrix::apply_pixel_mapper()`, which then presents you with a canvas
/// that has the new "visible width" and "visible height".
pub(crate) enum PixelMapper {
    Multiplex(MultiplexMapper),
    Named(Box<dyn NamedPixelMapper>),
}

impl PixelMapper {
    /// Given a underlying matrix (width, height), returns the
    /// visible (width, height) after the mapping.
    /// E.g. a 90 degree rotation might map matrix=(64, 32) -> visible=(32, 64)
    /// Some multiplexing matrices will double the height and half the width.
    pub(crate) fn get_size_mapping(
        &self,
        matrix_width: usize,
        matrix_height: usize,
    ) -> Result<[usize; 2], MatrixCreationError> {
        match self {
            PixelMapper::Multiplex(mapper) => mapper.get_size_mapping(matrix_width, matrix_height),
            PixelMapper::Named(mapper) => mapper.get_size_mapping(matrix_width, matrix_height),
        }
    }

    /// Map where a visible pixel (x,y) is mapped to the underlying matrix (x,y), or `None` if the mapper can't
    /// map it, e.g. because the panel is larger than the layout of a multiplexer.
    pub(crate) fn map_visible_to_matrix(
        &self,
        matrix_width: usize,
        matrix_height: usize,
        visible_x: usize,
        visible_y: usize,
    ) -> Option<[usize; 2]> {
        match self {
            PixelMapper::Multiplex(mapper) => {
                mapper.map_visible_to_matrix(matrix_width, matrix_height, visible_x, visible_y)
            }
            PixelMapper::Named(mapper) => Some(mapper.map_visible_to_matrix(
                matrix_width,
                matrix_height,
                visible_x,
                visible_y,
            )),
        }
    }

    pub(crate) fn mapping_direction(&self) -> MappingDirection {
        match self {
            PixelMapper::Multiplex(_) => MappingDirection::VisibleToMatrix,
            PixelMapper::Named(mapper) => mapper.mapping_direction(),
        }
    }

    /// Map where an underlying matrix pixel (x,y) is visible, if at all.
    pub(crate) fn map_matrix_to_visible(
        &self,
        matrix_width: usize,
        matrix_height: usize,
        matrix_x: usize,
        matrix_y: usize,
    ) -> Option<[usize; 2]> {
        match self {
            PixelMapper::Multiplex(_) => None,
            PixelMapper::Named(mapper) => {
                mapper.map_matrix_to_visible(matrix_width, matrix_height, matrix_x, matrix_y)
            }
        }
    }
}
