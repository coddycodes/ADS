//! The simulator's orientation and its framebuffer pixels turned upright. The framebuffer
//! IOSurface always stays portrait while the guest draws rotated content into it (spec §0,
//! S3), so every frame is turned into the UI orientation on the server.

use crate::device::catalog::Orientation;

/// The guest UI's orientation, as `-[SimScreenProperties uiOrientation]` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IosOrientation {
    Portrait,
    ReversePortrait,
    Landscape,
    ReverseLandscape,
}

/// How the portrait framebuffer turns to show the UI upright.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Turn {
    None,
    /// 90° clockwise: the surface's left edge becomes the top.
    Clockwise,
    /// 90° counter-clockwise: the surface's right edge becomes the top.
    CounterClockwise,
    Half,
}

impl IosOrientation {
    /// `uiOrientation` 1 is portrait and 2 upside-down. Which of 3 and 4 BiBCode calls
    /// `landscape` is provisional: Task 8's real-simulator rotate test confirms it against
    /// Android's convention. Other values are not orientations.
    pub(crate) fn from_ui(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Portrait),
            2 => Some(Self::ReversePortrait),
            3 => Some(Self::Landscape),
            4 => Some(Self::ReverseLandscape),
            _ => None,
        }
    }

    /// The orientation the catalog shows.
    pub(crate) fn orientation(self) -> Orientation {
        match self {
            Self::Portrait => Orientation::Portrait,
            Self::ReversePortrait => Orientation::ReversePortrait,
            Self::Landscape => Orientation::Landscape,
            Self::ReverseLandscape => Orientation::ReverseLandscape,
        }
    }

    /// Follows the `uiOrientation` value, not the catalog name: spec §0 observed the UI's top
    /// along the surface's physical left edge at 3 and its right edge at 4. If Task 8 swaps
    /// `from_ui`'s 3 and 4, this swaps with it; a test pins both together.
    pub(crate) fn turn(self) -> Turn {
        match self {
            Self::Portrait => Turn::None,
            Self::ReversePortrait => Turn::Half,
            Self::Landscape => Turn::Clockwise,
            Self::ReverseLandscape => Turn::CounterClockwise,
        }
    }
}

/// A locked BGRA framebuffer: `height` rows of `bytes_per_row` bytes, each starting with
/// `width` four-byte pixels.
pub(crate) struct Bgra<'a> {
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub bytes_per_row: usize,
}

/// Turns `source` upright for `turn`, scales it down to at most `max_width` (nearest
/// neighbour), and converts BGRA to RGB in the same pass. Returns the RGB bytes and their
/// width and height; `None` when the buffer is smaller than its geometry says.
pub(crate) fn upright_rgb(
    source: &Bgra<'_>,
    turn: Turn,
    max_width: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    let (width, height) = (source.width as usize, source.height as usize);
    if width == 0 || height == 0 || source.bytes_per_row < width * 4 {
        return None;
    }
    let needed = (height - 1) * source.bytes_per_row + width * 4;
    if source.pixels.len() < needed {
        return None;
    }
    // The upright size, before scaling.
    let (upright_width, upright_height) = match turn {
        Turn::None | Turn::Half => (width, height),
        Turn::Clockwise | Turn::CounterClockwise => (height, width),
    };
    let out_width = upright_width.min(max_width.max(1) as usize);
    let out_height = (upright_height * out_width).div_ceil(upright_width).max(1);
    // Each output pixel's source offset is `columns[x] + rows[y]`: every turn maps an
    // output column to one source axis and an output row to the other.
    let column = |x: usize| x * upright_width / out_width;
    let row = |y: usize| y * upright_height / out_height;
    let bpr = source.bytes_per_row;
    let (columns, rows): (Vec<usize>, Vec<usize>) = match turn {
        // Upright (u, v) reads the source at (u, v).
        Turn::None => (
            (0..out_width).map(|x| column(x) * 4).collect(),
            (0..out_height).map(|y| row(y) * bpr).collect(),
        ),
        // The source's left edge is the top: (u, v) reads (v, height - 1 - u).
        Turn::Clockwise => (
            (0..out_width)
                .map(|x| (height - 1 - column(x)) * bpr)
                .collect(),
            (0..out_height).map(|y| row(y) * 4).collect(),
        ),
        // The source's right edge is the top: (u, v) reads (width - 1 - v, u).
        Turn::CounterClockwise => (
            (0..out_width).map(|x| column(x) * bpr).collect(),
            (0..out_height).map(|y| (width - 1 - row(y)) * 4).collect(),
        ),
        // (u, v) reads (width - 1 - u, height - 1 - v).
        Turn::Half => (
            (0..out_width)
                .map(|x| (width - 1 - column(x)) * 4)
                .collect(),
            (0..out_height)
                .map(|y| (height - 1 - row(y)) * bpr)
                .collect(),
        ),
    };
    let mut rgb = Vec::with_capacity(out_width * out_height * 3);
    for &row_offset in &rows {
        for &column_offset in &columns {
            let at = row_offset + column_offset;
            let pixel = &source.pixels[at..at + 4];
            rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
    }
    let (out_width, out_height) = (
        u32::try_from(out_width).ok()?,
        u32::try_from(out_height).ok()?,
    );
    Some((rgb, out_width, out_height))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3×2 BGRA image whose pixel at (x, y) has blue `10 * y + x`, padded to 16-byte rows.
    fn sample() -> Vec<u8> {
        let mut pixels = vec![0xEE; 16 * 2];
        for y in 0..2 {
            for x in 0..3 {
                let at = y * 16 + x * 4;
                pixels[at..at + 4].copy_from_slice(&[(10 * y + x) as u8, 0, 0, 0xFF]);
            }
        }
        pixels
    }

    /// The blue value of each RGB pixel, row by row.
    fn blues(rgb: &[u8]) -> Vec<u8> {
        rgb.chunks(3).map(|pixel| pixel[2]).collect()
    }

    fn turned(turn: Turn) -> (Vec<u8>, u32, u32) {
        let pixels = sample();
        let source = Bgra {
            pixels: &pixels,
            width: 3,
            height: 2,
            bytes_per_row: 16,
        };
        let (rgb, width, height) = upright_rgb(&source, turn, 100).unwrap();
        (blues(&rgb), width, height)
    }

    #[test]
    fn maps_ui_orientation_values() {
        assert_eq!(IosOrientation::from_ui(1), Some(IosOrientation::Portrait));
        assert_eq!(
            IosOrientation::from_ui(2),
            Some(IosOrientation::ReversePortrait)
        );
        assert_eq!(IosOrientation::from_ui(3), Some(IosOrientation::Landscape));
        assert_eq!(
            IosOrientation::from_ui(4),
            Some(IosOrientation::ReverseLandscape)
        );
        assert_eq!(IosOrientation::from_ui(0), None);
        assert_eq!(IosOrientation::from_ui(5), None);
        assert_eq!(
            IosOrientation::from_ui(3).unwrap().orientation(),
            Orientation::Landscape
        );
    }

    #[test]
    fn the_turn_follows_the_observed_ui_value() {
        // Spec §0: at 3 the UI's top lies along the surface's left edge, at 4 its right.
        assert_eq!(IosOrientation::from_ui(1).unwrap().turn(), Turn::None);
        assert_eq!(IosOrientation::from_ui(2).unwrap().turn(), Turn::Half);
        assert_eq!(IosOrientation::from_ui(3).unwrap().turn(), Turn::Clockwise);
        assert_eq!(
            IosOrientation::from_ui(4).unwrap().turn(),
            Turn::CounterClockwise
        );
    }

    #[test]
    fn honours_the_row_stride_and_swaps_channels() {
        assert_eq!(turned(Turn::None), (vec![0, 1, 2, 10, 11, 12], 3, 2));
        let pixels = sample();
        let source = Bgra {
            pixels: &pixels,
            width: 3,
            height: 2,
            bytes_per_row: 16,
        };
        let (rgb, _, _) = upright_rgb(&source, Turn::None, 100).unwrap();
        assert_eq!(&rgb[..3], &[0, 0, 0], "B and R swap; padding is never read");
    }

    #[test]
    fn turns_into_each_orientation() {
        // Clockwise: the left column (0, 10) becomes the top row, read right to left.
        assert_eq!(turned(Turn::Clockwise), (vec![10, 0, 11, 1, 12, 2], 2, 3));
        // Counter-clockwise: the right column (2, 12) becomes the top row.
        assert_eq!(
            turned(Turn::CounterClockwise),
            (vec![2, 12, 1, 11, 0, 10], 2, 3)
        );
        assert_eq!(turned(Turn::Half), (vec![12, 11, 10, 2, 1, 0], 3, 2));
    }

    #[test]
    fn landscape_frames_are_wider_than_tall_and_scaled() {
        let (width, height, bpr) = (1206usize, 2622usize, 4864usize);
        let pixels = vec![0x40; bpr * height];
        let source = Bgra {
            pixels: &pixels,
            width: width as u32,
            height: height as u32,
            bytes_per_row: bpr,
        };
        let (rgb, w, h) = upright_rgb(&source, Turn::Clockwise, 720).unwrap();
        assert_eq!((w, h), (720, 332));
        assert_eq!(rgb.len(), 720 * 332 * 3);
        let (_, w, h) = upright_rgb(&source, Turn::None, 720).unwrap();
        assert_eq!((w, h), (720, 1566));
    }

    #[test]
    fn rejects_a_buffer_smaller_than_its_geometry() {
        let pixels = vec![0; 16];
        let source = Bgra {
            pixels: &pixels,
            width: 3,
            height: 2,
            bytes_per_row: 16,
        };
        assert!(upright_rgb(&source, Turn::None, 100).is_none());
        let narrow = Bgra {
            pixels: &pixels,
            width: 5,
            height: 1,
            bytes_per_row: 16,
        };
        assert!(upright_rgb(&narrow, Turn::None, 100).is_none());
    }
}
