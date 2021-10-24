/*!
A high-level, safe, zero-allocation TrueType font parser.

## Features

- A high-level API for most common features, hiding all parsing and data resolving logic.
- A low-level API for for some complex TrueType tables, still hiding raw data layout.
- Zero heap allocations.
- Zero unsafe.
- Zero dependencies.
- `no_std`/WASM compatible.
- Fast.
- Stateless. All parsing methods are immutable methods.
- Simple and maintainable code (no magic numbers).

## Safety

- The library must not panic. Any panic considered as a critical bug and should be reported.
- The library forbids the unsafe code.
- No heap allocations, so crash due to OOM is not possible.
- All recursive methods have a depth limit.
- Technically, should use less than 64KiB of stack in worst case scenario.
- Most of arithmetic operations are checked.
- Most of numeric casts are checked.
*/

#![doc(html_root_url = "https://docs.rs/ttf-parser/0.12.3")]

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(missing_copy_implementations)]
#![warn(missing_debug_implementations)]

#[cfg(feature = "std")]
#[macro_use]
extern crate std;

use core::num::NonZeroU16;
use core::ops::{Deref, DerefMut};

macro_rules! try_opt_or {
    ($value:expr, $ret:expr) => {
        match $value {
            Some(v) => v,
            None => return $ret,
        }
    };
}

pub mod parser;
mod tables;
#[cfg(feature = "opentype-layout")] mod ggg;
#[cfg(feature = "variable-fonts")] mod var_store;

use tables::*;
use parser::{Stream, FromData, NumFrom, TryNumFrom, LazyArray16, LazyArrayIter16, Offset32, Offset};
use head::IndexToLocationFormat;

#[cfg(feature = "variable-fonts")] pub use fvar::{VariationAxes, VariationAxis};

pub use name::{name_id, PlatformId};
pub use os2::{Weight, Width, ScriptMetrics, Style};
pub use tables::CFFError;
pub use tables::{cmap, kern, sbix, maxp, hmtx, name, os2, loca, svg, vorg, post, head, hhea, glyf};
pub use tables::{cff1 as cff, vhea};
#[cfg(feature = "variable-fonts")] pub use tables::{cff2, avar};

#[cfg(feature = "opentype-layout")]
pub mod opentype_layout {
    //! This module contains
    //! [OpenType Layout](https://docs.microsoft.com/en-us/typography/opentype/spec/chapter2#overview)
    //! tables implementation.
    //!
    //! Unlike the main [`Face`](crate::Face) API, this module is very low-level
    //! and contains only parsing abstraction without any logic on top of it.

    pub use crate::gdef::*;
    pub use crate::tables::gpos as positioning;
    pub use crate::tables::gsub as substitution;
    pub use crate::ggg::*;
}


/// A type-safe wrapper for glyph ID.
#[repr(transparent)]
#[derive(Clone, Copy, Ord, PartialOrd, Eq, PartialEq, Default, Debug, Hash)]
pub struct GlyphId(pub u16);

impl FromData for GlyphId {
    const SIZE: usize = 2;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        u16::parse(data).map(GlyphId)
    }
}


/// A TrueType font magic.
///
/// https://docs.microsoft.com/en-us/typography/opentype/spec/otff#organization-of-an-opentype-font
#[derive(Clone, Copy, PartialEq, Debug)]
enum Magic {
    TrueType,
    OpenType,
    FontCollection,
}

impl FromData for Magic {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        match u32::parse(data)? {
            0x00010000 | 0x74727565 => Some(Magic::TrueType),
            0x4F54544F => Some(Magic::OpenType),
            0x74746366 => Some(Magic::FontCollection),
            _ => None,
        }
    }
}


/// A variation coordinate in a normalized coordinate system.
///
/// Basically any number in a -1.0..1.0 range.
/// Where 0 is a default value.
///
/// The number is stored as f2.16
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct NormalizedCoordinate(i16);

impl From<i16> for NormalizedCoordinate {
    /// Creates a new coordinate.
    ///
    /// The provided number will be clamped to the -16384..16384 range.
    #[inline]
    fn from(n: i16) -> Self {
        NormalizedCoordinate(parser::i16_bound(-16384, n, 16384))
    }
}

impl From<f32> for NormalizedCoordinate {
    /// Creates a new coordinate.
    ///
    /// The provided number will be clamped to the -1.0..1.0 range.
    #[inline]
    fn from(n: f32) -> Self {
        NormalizedCoordinate((parser::f32_bound(-1.0, n, 1.0) * 16384.0) as i16)
    }
}

impl NormalizedCoordinate {
    /// Returns the coordinate value as f2.14.
    #[inline]
    pub fn get(self) -> i16 {
        self.0
    }
}


/// A font variation value.
///
/// # Example
///
/// ```
/// use ttf_parser::{Variation, Tag};
///
/// Variation { axis: Tag::from_bytes(b"wght"), value: 500.0 };
/// ```
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Variation {
    /// An axis tag name.
    pub axis: Tag,
    /// An axis value.
    pub value: f32,
}


/// A 4-byte tag.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag(pub u32);

impl Tag {
    /// Creates a `Tag` from bytes.
    #[inline]
    pub const fn from_bytes(bytes: &[u8; 4]) -> Self {
        Tag(((bytes[0] as u32) << 24) | ((bytes[1] as u32) << 16) |
            ((bytes[2] as u32) << 8) | (bytes[3] as u32))
    }

    /// Creates a `Tag` from bytes.
    ///
    /// In case of empty data will return `Tag` set to 0.
    ///
    /// When `bytes` are shorter than 4, will set missing bytes to ` `.
    ///
    /// Data after first 4 bytes is ignored.
    #[inline]
    pub fn from_bytes_lossy(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Tag::from_bytes(&[0, 0, 0, 0]);
        }

        let mut iter = bytes.iter().cloned().chain(core::iter::repeat(b' '));
        Tag::from_bytes(&[
            iter.next().unwrap(),
            iter.next().unwrap(),
            iter.next().unwrap(),
            iter.next().unwrap(),
        ])
    }

    /// Returns tag as 4-element byte array.
    #[inline]
    pub const fn to_bytes(self) -> [u8; 4] {
        [
            (self.0 >> 24 & 0xff) as u8,
            (self.0 >> 16 & 0xff) as u8,
            (self.0 >> 8 & 0xff) as u8,
            (self.0 >> 0 & 0xff) as u8,
        ]
    }

    /// Returns tag as 4-element byte array.
    #[inline]
    pub const fn to_chars(self) -> [char; 4] {
        [
            (self.0 >> 24 & 0xff) as u8 as char,
            (self.0 >> 16 & 0xff) as u8 as char,
            (self.0 >> 8 & 0xff) as u8 as char,
            (self.0 >> 0 & 0xff) as u8 as char,
        ]
    }

    /// Checks if tag is null / `[0, 0, 0, 0]`.
    #[inline]
    pub const fn is_null(&self) -> bool {
        self.0 == 0
    }

    /// Returns tag value as `u32` number.
    #[inline]
    pub const fn as_u32(&self) -> u32 {
        self.0
    }
}

impl core::fmt::Debug for Tag {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Tag({})", self)
    }
}

impl core::fmt::Display for Tag {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let b = self.to_chars();
        write!(
            f,
            "{}{}{}{}",
            b.get(0).unwrap_or(&' '),
            b.get(1).unwrap_or(&' '),
            b.get(2).unwrap_or(&' '),
            b.get(3).unwrap_or(&' ')
        )
    }
}

impl FromData for Tag {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        u32::parse(data).map(Tag)
    }
}


/// A line metrics.
///
/// Used for underline and strikeout.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LineMetrics {
    /// Line position.
    pub position: i16,

    /// Line thickness.
    pub thickness: i16,
}


/// A rectangle.
///
/// Doesn't guarantee that `x_min` <= `x_max` and/or `y_min` <= `y_max`.
#[repr(C)]
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

impl Rect {
    /// Returns rect's width.
    #[inline]
    pub fn width(&self) -> i16 {
        self.x_max - self.x_min
    }

    /// Returns rect's height.
    #[inline]
    pub fn height(&self) -> i16 {
        self.y_max - self.y_min
    }
}


#[derive(Clone, Copy, Debug)]
pub(crate) struct BBox {
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
}

impl BBox {
    #[inline]
    fn new() -> Self {
        BBox {
            x_min: core::f32::MAX,
            y_min: core::f32::MAX,
            x_max: core::f32::MIN,
            y_max: core::f32::MIN,
        }
    }

    #[inline]
    fn is_default(&self) -> bool {
        self.x_min == core::f32::MAX &&
        self.y_min == core::f32::MAX &&
        self.x_max == core::f32::MIN &&
        self.y_max == core::f32::MIN
    }

    #[inline]
    fn extend_by(&mut self, x: f32, y: f32) {
        self.x_min = self.x_min.min(x);
        self.y_min = self.y_min.min(y);
        self.x_max = self.x_max.max(x);
        self.y_max = self.y_max.max(y);
    }

    #[inline]
    fn to_rect(&self) -> Option<Rect> {
        Some(Rect {
            x_min: i16::try_num_from(self.x_min)?,
            y_min: i16::try_num_from(self.y_min)?,
            x_max: i16::try_num_from(self.x_max)?,
            y_max: i16::try_num_from(self.y_max)?,
        })
    }
}


/// A trait for glyph outline construction.
pub trait OutlineBuilder {
    /// Appends a MoveTo segment.
    ///
    /// Start of a contour.
    fn move_to(&mut self, x: f32, y: f32);

    /// Appends a LineTo segment.
    fn line_to(&mut self, x: f32, y: f32);

    /// Appends a QuadTo segment.
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32);

    /// Appends a CurveTo segment.
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32);

    /// Appends a ClosePath segment.
    ///
    /// End of a contour.
    fn close(&mut self);
}


struct DummyOutline;
impl OutlineBuilder for DummyOutline {
    fn move_to(&mut self, _: f32, _: f32) {}
    fn line_to(&mut self, _: f32, _: f32) {}
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
    fn close(&mut self) {}
}


/// A glyph raster image format.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RasterImageFormat {
    PNG,
}


/// A glyph's raster image.
///
/// Note, that glyph metrics are in pixels and not in font units.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RasterGlyphImage<'a> {
    /// Horizontal offset.
    pub x: i16,

    /// Vertical offset.
    pub y: i16,

    /// Image width.
    ///
    /// It doesn't guarantee that this value is the same as set in the `data`.
    pub width: u16,

    /// Image height.
    ///
    /// It doesn't guarantee that this value is the same as set in the `data`.
    pub height: u16,

    /// A pixels per em of the selected strike.
    pub pixels_per_em: u16,

    /// An image format.
    pub format: RasterImageFormat,

    /// A raw image data. It's up to the caller to decode it.
    pub data: &'a [u8],
}


/// A table name.
#[repr(C)]
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TableName {
    AxisVariations = 0,
    CharacterToGlyphIndexMapping,
    ColorBitmapData,
    ColorBitmapLocation,
    CompactFontFormat,
    CompactFontFormat2,
    FontVariations,
    GlyphData,
    GlyphDefinition,
    GlyphPositioningTable,
    GlyphSubstitutionTable,
    GlyphVariations,
    Header,
    HorizontalHeader,
    HorizontalMetrics,
    HorizontalMetricsVariations,
    IndexToLocation,
    Kerning,
    MaximumProfile,
    MetricsVariations,
    Naming,
    PostScript,
    ScalableVectorGraphics,
    StandardBitmapGraphics,
    VerticalHeader,
    VerticalMetrics,
    VerticalMetricsVariations,
    VerticalOrigin,
    WindowsMetrics,
}


#[derive(Clone, Copy)]
struct TableRecord {
    table_tag: Tag,
    #[allow(dead_code)]
    check_sum: u32,
    offset: u32,
    length: u32,
}

impl FromData for TableRecord {
    const SIZE: usize = 16;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(TableRecord {
            table_tag: s.read::<Tag>()?,
            check_sum: s.read::<u32>()?,
            offset: s.read::<u32>()?,
            length: s.read::<u32>()?,
        })
    }
}


#[cfg(feature = "variable-fonts")]
const MAX_VAR_COORDS: usize = 32;

#[cfg(feature = "variable-fonts")]
#[derive(Clone, Default)]
struct VarCoords {
    data: [NormalizedCoordinate; MAX_VAR_COORDS],
    len: u8,
}

#[cfg(feature = "variable-fonts")]
impl VarCoords {
    #[inline]
    fn as_slice(&self) -> &[NormalizedCoordinate] {
        &self.data[0..usize::from(self.len)]
    }

    #[inline]
    fn as_mut_slice(&mut self) -> &mut [NormalizedCoordinate] {
        let end = usize::from(self.len);
        &mut self.data[0..end]
    }
}


/// A list of font face parsing errors.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FaceParsingError {
    /// An attempt to read out of bounds detected.
    ///
    /// Should occur only on malformed fonts.
    MalformedFont,

    /// Face data must start with `0x00010000`, `0x74727565`, `0x4F54544F` or `0x74746366`.
    UnknownMagic,

    /// The face index is larger than the number of faces in the font.
    FaceIndexOutOfBounds,

    /// The `head` table is missing or malformed.
    NoHeadTable,

    /// The `hhea` table is missing or malformed.
    NoHheaTable,

    /// The `maxp` table is missing or malformed.
    NoMaxpTable,
}

impl core::fmt::Display for FaceParsingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FaceParsingError::MalformedFont => write!(f, "malformed font"),
            FaceParsingError::UnknownMagic => write!(f, "unknown magic"),
            FaceParsingError::FaceIndexOutOfBounds => write!(f, "face index is out of bounds"),
            FaceParsingError::NoHeadTable => write!(f, "the head table is missing or malformed"),
            FaceParsingError::NoHheaTable => write!(f, "the hhea table is missing or malformed"),
            FaceParsingError::NoMaxpTable => write!(f, "the maxp table is missing or malformed"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for FaceParsingError {}


/// A font face handle.
///
/// While `Face` is technically copyable, we disallow it because it's almost 2KB big.
#[derive(Clone)]
pub struct Face<'a> {
    font_data: &'a [u8], // The input data. Used by Face::table_data.
    table_records: LazyArray16<'a, TableRecord>,
    tables: FaceTables<'a>,
}

impl<'a> Deref for Face<'a> {
    type Target = FaceTables<'a>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.tables
    }
}

impl<'a> DerefMut for Face<'a> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tables
    }
}

/// Parsed face tables.
///
/// This struct adds the `from_table_provider()` method that is not
/// available on the `Face`. You can create a `FaceTables` struct
/// from your own, custom font provider. This is important if your font
/// provider does things that ttf-parser currently doesn't implement
/// (for example zlib / brotli decoding)
#[derive(Clone)]
pub struct FaceTables<'a> {
    cbdt: Option<&'a [u8]>,
    cblc: Option<&'a [u8]>,
    cff: Option<cff1::Table<'a>>,
    cmap: Option<cmap::Table<'a>>,
    glyf: Option<glyf::Table<'a>>,
    head: head::Table,
    hhea: hhea::Table,
    hmtx: Option<hmtx::Table<'a>>,
    kern: Option<kern::Table<'a>>,
    maxp: maxp::Table,
    name: Option<name::Table<'a>>,
    os_2: Option<os2::Table<'a>>,
    post: Option<post::Table<'a>>,
    vhea: Option<vhea::Table>,
    vmtx: Option<hmtx::Table<'a>>,
    sbix: Option<sbix::Table<'a>>,
    svg: Option<svg::Table<'a>>,
    vorg: Option<vorg::Table<'a>>,

    #[cfg(feature = "opentype-layout")] gdef: Option<gdef::DefinitionTable<'a>>,
    #[cfg(feature = "opentype-layout")] gpos: Option<ggg::LayoutTable<'a>>,
    #[cfg(feature = "opentype-layout")] gsub: Option<ggg::LayoutTable<'a>>,

    // Variable font tables.
    #[cfg(feature = "variable-fonts")] avar: Option<avar::Table<'a>>,
    #[cfg(feature = "variable-fonts")] cff2: Option<cff2::Table<'a>>,
    #[cfg(feature = "variable-fonts")] fvar: Option<fvar::Table<'a>>,
    #[cfg(feature = "variable-fonts")] gvar: Option<gvar::Table<'a>>,
    #[cfg(feature = "variable-fonts")] hvar: Option<hvar::Table<'a>>,
    #[cfg(feature = "variable-fonts")] mvar: Option<mvar::Table<'a>>,
    #[cfg(feature = "variable-fonts")] vvar: Option<hvar::Table<'a>>,

    #[cfg(feature = "variable-fonts")] coordinates: VarCoords,
}

impl core::fmt::Debug for FaceTables<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "FaceTables()")
    }
}

impl<'a> Face<'a> {
    /// Creates a new `Face` object from a raw data.
    ///
    /// `index` indicates the specific font face in a font collection.
    /// Use `fonts_in_collection` to get the total number of font faces.
    /// Set to 0 if unsure.
    ///
    /// This method will do some parsing and sanitization, so it's a bit expensive.
    ///
    /// Required tables: `head`, `hhea` and `maxp`.
    ///
    /// If an optional table has invalid data it will be skipped.
    pub fn from_slice(data: &'a [u8], index: u32) -> Result<Self, FaceParsingError> {
        // https://docs.microsoft.com/en-us/typography/opentype/spec/otff#organization-of-an-opentype-font

        let mut s = Stream::new(data);

        // Read **font** magic.
        let magic: Magic = s.read().ok_or(FaceParsingError::UnknownMagic)?;
        if magic == Magic::FontCollection {
            s.skip::<u32>(); // version
            let number_of_faces: u32 = s.read().ok_or(FaceParsingError::MalformedFont)?;
            let offsets = s.read_array32::<Offset32>(number_of_faces)
                .ok_or(FaceParsingError::MalformedFont)?;

            let face_offset = offsets.get(index).ok_or(FaceParsingError::FaceIndexOutOfBounds)?;
            // Face offset is from the start of the font data,
            // so we have to adjust it to the current parser offset.
            let face_offset = face_offset.to_usize().checked_sub(s.offset())
                .ok_or(FaceParsingError::MalformedFont)?;
            s.advance_checked(face_offset).ok_or(FaceParsingError::MalformedFont)?;

            // Read **face** magic.
            // Each face in a font collection also starts with a magic.
            let magic: Magic = s.read().ok_or(FaceParsingError::UnknownMagic)?;
            // And face in a font collection can't be another collection.
            if magic == Magic::FontCollection {
                return Err(FaceParsingError::UnknownMagic);
            }
        }

        let num_tables: u16 = s.read().ok_or(FaceParsingError::MalformedFont)?;
        s.advance(6); // searchRange (u16) + entrySelector (u16) + rangeShift (u16)
        let table_records = s.read_array16::<TableRecord>(num_tables)
            .ok_or(FaceParsingError::MalformedFont)?;

        let tables = FaceTables::from_table_provider(
            DefaultTableProvider {
                tables: table_records.into_iter(),
                data
            }
        )?;

        Ok(Face {
            font_data: data,
            table_records: table_records,
            tables,
        })
    }

    /// Returns the raw data of a selected table.
    ///
    /// Useful if you want to parse the data manually.
    pub fn table_data(&self, tag: Tag) -> Option<&'a [u8]> {
        let (_, table) = self.table_records.binary_search_by(|record| record.table_tag.cmp(&tag))?;
        let offset = usize::num_from(table.offset);
        let length = usize::num_from(table.length);
        let end = offset.checked_add(length)?;
        self.font_data.get(offset..end)
    }
}

impl<'a> FaceTables<'a> {
    /// Creates and parses face tables from an existing table provider.
    ///
    /// This is useful for integrating `ttf-parser` with other font-parsing
    /// libraries that already do table decoding
    pub fn from_table_provider<T>(provider: T) -> Result<Self, FaceParsingError>
        where T: Iterator<Item=Result<(Tag, Option<&'a [u8]>), FaceParsingError>>
    {
        let mut face = FaceTables {
            cbdt: None,
            cblc: None,
            cff: None,
            cmap: None,
            #[cfg(feature = "opentype-layout")] gdef: None,
            #[cfg(feature = "opentype-layout")] gpos: None,
            #[cfg(feature = "opentype-layout")] gsub: None,
            glyf: None,
            head: head::Table::default(), // temporary
            hhea: hhea::Table::default(), // temporary
            hmtx: None,
            kern: None,
            maxp: maxp::Table { number_of_glyphs: NonZeroU16::new(1).unwrap() }, // temporary
            name: None,
            os_2: None,
            post: None,
            vhea: None,
            vmtx: None,
            sbix: None,
            svg: None,
            vorg: None,
            #[cfg(feature = "variable-fonts")] avar: None,
            #[cfg(feature = "variable-fonts")] cff2: None,
            #[cfg(feature = "variable-fonts")] fvar: None,
            #[cfg(feature = "variable-fonts")] gvar: None,
            #[cfg(feature = "variable-fonts")] hvar: None,
            #[cfg(feature = "variable-fonts")] mvar: None,
            #[cfg(feature = "variable-fonts")] vvar: None,
            #[cfg(feature = "variable-fonts")] coordinates: VarCoords::default(),
        };

        let mut hhea: &[u8] = &[];
        let mut maxp = None;
        let mut hmtx = None;
        let mut vmtx = None;
        let mut loca = None;
        let mut glyf = None;
        let mut sbix = None;

        for table_tag_table_data in provider {
            let (table_tag, table_data) = table_tag_table_data?;
            match &table_tag.to_bytes() {
                b"CBDT" => face.cbdt = table_data,
                b"CBLC" => face.cblc = table_data,
                b"CFF " => face.cff = table_data.and_then(|data| cff::Table::parse(data)),
                #[cfg(feature = "variable-fonts")]
                b"CFF2" => face.cff2 = table_data.and_then(|data| cff2::Table::parse(data)),
                #[cfg(feature = "opentype-layout")]
                b"GDEF" => face.gdef = table_data.and_then(|data| gdef::DefinitionTable::parse(data)),
                #[cfg(feature = "opentype-layout")]
                b"GPOS" => face.gpos = table_data.and_then(|data| ggg::LayoutTable::parse(data)),
                #[cfg(feature = "opentype-layout")]
                b"GSUB" => face.gsub = table_data.and_then(|data| ggg::LayoutTable::parse(data)),
                #[cfg(feature = "variable-fonts")]
                b"HVAR" => face.hvar = table_data.and_then(|data| hvar::Table::parse(data)),
                #[cfg(feature = "variable-fonts")]
                b"MVAR" => face.mvar = table_data.and_then(|data| mvar::Table::parse(data)),
                b"OS/2" => face.os_2 = table_data.and_then(|data| os2::Table::parse(data)),
                b"SVG " => face.svg = table_data.and_then(|data| svg::Table::parse(data)),
                b"VORG" => face.vorg = table_data.and_then(|data| vorg::Table::parse(data)),
                #[cfg(feature = "variable-fonts")]
                b"VVAR" => face.vvar = table_data.and_then(|data| hvar::Table::parse(data)),
                #[cfg(feature = "variable-fonts")]
                b"avar" => face.avar = table_data.and_then(|data| avar::Table::parse(data)),
                b"cmap" => face.cmap = table_data.and_then(|data| cmap::Table::parse(data)),
                #[cfg(feature = "variable-fonts")]
                b"fvar" => face.fvar = table_data.and_then(|data| fvar::Table::parse(data)),
                b"glyf" => glyf = table_data,
                #[cfg(feature = "variable-fonts")]
                b"gvar" => face.gvar = table_data.and_then(|data| gvar::Table::parse(data)),
                b"head" => face.head = table_data.and_then(|data| head::Table::parse(data)).unwrap_or_default(),
                b"hhea" => hhea = table_data.unwrap_or_default(),
                b"hmtx" => hmtx = table_data,
                b"kern" => face.kern = table_data.and_then(|data| kern::Table::parse(data)),
                b"loca" => loca = table_data,
                b"maxp" => maxp = table_data.and_then(|data| maxp::Table::parse(data)),
                b"name" => face.name = table_data.and_then(|data| name::Table::parse(data)),
                b"post" => face.post = table_data.and_then(|data| post::Table::parse(data)),
                b"sbix" => sbix = table_data,
                b"vhea" => face.vhea = table_data.and_then(|data| vhea::Table::parse(data)),
                b"vmtx" => vmtx = table_data,
                _ => {}
            }
        }

        if face.head.units_per_em == 0 {
            return Err(FaceParsingError::NoHeadTable);
        }

        face.hhea = hhea::Table::parse(hhea).ok_or(FaceParsingError::NoHheaTable)?;

        face.maxp = match maxp {
            Some(n) => n,
            None => return Err(FaceParsingError::NoMaxpTable),
        };

        #[cfg(feature = "variable-fonts")] {
            if let Some(ref fvar) = face.fvar {
                face.coordinates.len = fvar.axes().count().min(MAX_VAR_COORDS) as u8;
            }
        }

        if let Some(data) = hmtx {
            face.hmtx = hmtx::Table::parse(
                data,
                face.hhea.number_of_metrics,
                face.maxp.number_of_glyphs,
            );
        }

        if let (Some(vhea), Some(data)) = (face.vhea, vmtx) {
            face.vmtx = hmtx::Table::parse(data, vhea.number_of_metrics, face.maxp.number_of_glyphs);
        }

        if let Some(data) = loca {
            let loca = loca::Table::parse(
                data,
                face.maxp.number_of_glyphs,
                face.head.index_to_location_format,
            );

            if let Some(loca_table) = loca {
                if let Some(glyf) = glyf {
                    face.glyf = Some(glyf::Table::parse(loca_table, glyf));
                }
            }
        }

        if let Some(data) = sbix {
            face.sbix = sbix::Table::parse(face.maxp.number_of_glyphs, data);
        }

        Ok(face)
    }

    /// Checks that face has a specified table.
    ///
    /// Will return `true` only for tables that were successfully parsed.
    #[inline]
    pub fn has_table(&self, name: TableName) -> bool {
        match name {
            TableName::Header                       => true,
            TableName::HorizontalHeader             => true,
            TableName::MaximumProfile               => true,
            TableName::AxisVariations               => {
                #[cfg(feature = "variable-fonts")] { self.avar.is_some() }
                #[cfg(not(feature = "variable-fonts"))] { false }
            }
            TableName::CharacterToGlyphIndexMapping => self.cmap.is_some(),
            TableName::ColorBitmapData              => self.cbdt.is_some(),
            TableName::ColorBitmapLocation          => self.cblc.is_some(),
            TableName::CompactFontFormat            => self.cff.is_some(),
            TableName::CompactFontFormat2           => {
                #[cfg(feature = "variable-fonts")] { self.cff2.is_some() }
                #[cfg(not(feature = "variable-fonts"))] { false }
            }
            TableName::FontVariations               => {
                #[cfg(feature = "variable-fonts")] { self.fvar.is_some() }
                #[cfg(not(feature = "variable-fonts"))] { false }
            }
            TableName::GlyphData                    => self.glyf.is_some(),
            TableName::GlyphDefinition              => {
                #[cfg(feature = "opentype-layout")] { self.gdef.is_some() }
                #[cfg(not(feature = "opentype-layout"))] { false }
            }
            TableName::GlyphPositioningTable        => {
                #[cfg(feature = "opentype-layout")] { self.gpos.is_some() }
                #[cfg(not(feature = "opentype-layout"))] { false }
            }
            TableName::GlyphSubstitutionTable       => {
                #[cfg(feature = "opentype-layout")] { self.gsub.is_some() }
                #[cfg(not(feature = "opentype-layout"))] { false }
            }
            TableName::GlyphVariations              => {
                #[cfg(feature = "variable-fonts")] { self.gvar.is_some() }
                #[cfg(not(feature = "variable-fonts"))] { false }
            }
            TableName::HorizontalMetrics            => self.hmtx.is_some(),
            TableName::HorizontalMetricsVariations  => {
                #[cfg(feature = "variable-fonts")] { self.hvar.is_some() }
                #[cfg(not(feature = "variable-fonts"))] { false }
            }
            TableName::IndexToLocation              => self.glyf.is_some(),
            TableName::Kerning                      => self.kern.is_some(),
            TableName::MetricsVariations            => {
                #[cfg(feature = "variable-fonts")] { self.mvar.is_some() }
                #[cfg(not(feature = "variable-fonts"))] { false }
            }
            TableName::Naming                       => self.name.is_some(),
            TableName::PostScript                   => self.post.is_some(),
            TableName::ScalableVectorGraphics       => self.svg.is_some(),
            TableName::StandardBitmapGraphics       => self.sbix.is_some(),
            TableName::VerticalHeader               => self.vhea.is_some(),
            TableName::VerticalMetrics              => self.vmtx.is_some(),
            TableName::VerticalMetricsVariations    => {
                #[cfg(feature = "variable-fonts")] { self.vvar.is_some() }
                #[cfg(not(feature = "variable-fonts"))] { false }
            }
            TableName::VerticalOrigin               => self.vorg.is_some(),
            TableName::WindowsMetrics               => self.os_2.is_some(),
        }
    }

    /// Returns an iterator over [Name Records].
    ///
    /// An iterator can be empty.
    ///
    /// [Name Records]: https://docs.microsoft.com/en-us/typography/opentype/spec/name#name-records
    #[inline]
    pub fn names(&self) -> name::Names<'a> {
        self.name.unwrap_or_default().names
    }

    /// Checks that face is marked as *Regular*.
    ///
    /// Returns `false` when OS/2 table is not present.
    #[inline]
    pub fn is_regular(&self) -> bool {
        self.os_2.map(|s| s.style() == Style::Normal).unwrap_or(false)
    }

    /// Checks that face is marked as *Italic*.
    ///
    /// Returns `false` when OS/2 table is not present.
    #[inline]
    pub fn is_italic(&self) -> bool {
        self.os_2.map(|s| s.style() == Style::Italic).unwrap_or(false)
    }

    /// Checks that face is marked as *Bold*.
    ///
    /// Returns `false` when OS/2 table is not present.
    #[inline]
    pub fn is_bold(&self) -> bool {
        try_opt_or!(self.os_2, false).is_bold()
    }

    /// Checks that face is marked as *Oblique*.
    ///
    /// Returns `false` when OS/2 table is not present or when its version is < 4.
    #[inline]
    pub fn is_oblique(&self) -> bool {
        self.os_2.map(|s| s.style() == Style::Oblique).unwrap_or(false)
    }

    /// Returns face style.
    #[inline]
    pub fn style(&self) -> Style {
        try_opt_or!(self.os_2, Style::Normal).style()
    }

    /// Checks that face is marked as *Monospaced*.
    ///
    /// Returns `false` when `post` table is not present.
    #[inline]
    pub fn is_monospaced(&self) -> bool {
        try_opt_or!(self.post, false).is_monospaced
    }

    /// Checks that face is variable.
    ///
    /// Simply checks the presence of a `fvar` table.
    #[inline]
    pub fn is_variable(&self) -> bool {
        #[cfg(feature = "variable-fonts")] {
            // `fvar::Table::parse` already checked that `axisCount` is non-zero.
            self.fvar.is_some()
        }

        #[cfg(not(feature = "variable-fonts"))] {
            false
        }
    }

    /// Returns face's weight.
    ///
    /// Returns `Weight::Normal` when OS/2 table is not present.
    #[inline]
    pub fn weight(&self) -> Weight {
        try_opt_or!(self.os_2, Weight::default()).weight()
    }

    /// Returns face's width.
    ///
    /// Returns `Width::Normal` when OS/2 table is not present or when value is invalid.
    #[inline]
    pub fn width(&self) -> Width {
        try_opt_or!(self.os_2, Width::default()).width()
    }

    /// Returns face's italic angle.
    ///
    /// Returns `None` when `post` table is not present.
    #[inline]
    pub fn italic_angle(&self) -> Option<f32> {
        self.post.map(|table| table.italic_angle)
    }

    // Read https://github.com/freetype/freetype/blob/49270c17011491227ec7bd3fb73ede4f674aa065/src/sfnt/sfobjs.c#L1279
    // to learn more about the logic behind the following functions.

    /// Returns a horizontal face ascender.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn ascender(&self) -> i16 {
        if let Some(os_2) = self.os_2 {
            if os_2.use_typographic_metrics() {
                let value = os_2.typographic_ascender();
                return self.apply_metrics_variation(Tag::from_bytes(b"hasc"), value);
            }
        }

        let mut value = self.hhea.ascender;
        if value == 0 {
            if let Some(os_2) = self.os_2 {
                value = os_2.typographic_ascender();
                if value == 0 {
                    value = os_2.windows_ascender();
                    value = self.apply_metrics_variation(Tag::from_bytes(b"hcla"), value);
                } else {
                    value = self.apply_metrics_variation(Tag::from_bytes(b"hasc"), value);
                }
            }
        }

        value
    }

    /// Returns a horizontal face descender.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn descender(&self) -> i16 {
        if let Some(os_2) = self.os_2 {
            if os_2.use_typographic_metrics() {
                let value = os_2.typographic_descender();
                return self.apply_metrics_variation(Tag::from_bytes(b"hdsc"), value);
            }
        }

        let mut value = self.hhea.descender;
        if value == 0 {
            if let Some(os_2) = self.os_2 {
                value = os_2.typographic_descender();
                if value == 0 {
                    value = os_2.windows_descender();
                    value = self.apply_metrics_variation(Tag::from_bytes(b"hcld"), value);
                } else {
                    value = self.apply_metrics_variation(Tag::from_bytes(b"hdsc"), value);
                }
            }
        }

        value
    }

    /// Returns face's height.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn height(&self) -> i16 {
        self.ascender() - self.descender()
    }

    /// Returns a horizontal face line gap.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn line_gap(&self) -> i16 {
        if let Some(os_2) = self.os_2 {
            if os_2.use_typographic_metrics() {
                let value = os_2.typographic_line_gap();
                return self.apply_metrics_variation(Tag::from_bytes(b"hlgp"), value);
            }
        }

        let mut value = self.hhea.line_gap;
        if value == 0 {
            if let Some(os_2) = self.os_2 {
                value = os_2.typographic_line_gap();
                value = self.apply_metrics_variation(Tag::from_bytes(b"hlgp"), value);
            }
        }

        value
    }

    /// Returns a horizontal typographic face ascender.
    ///
    /// Prefer `Face::ascender` unless you explicitly want this. This is a more
    /// low-level alternative.
    ///
    /// This method is affected by variation axes.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn typographic_ascender(&self) -> Option<i16> {
        self.os_2.map(|table| {
            let v = table.typographic_ascender();
            self.apply_metrics_variation(Tag::from_bytes(b"hasc"), v)
        })
    }

    /// Returns a horizontal typographic face descender.
    ///
    /// Prefer `Face::descender` unless you explicitly want this. This is a more
    /// low-level alternative.
    ///
    /// This method is affected by variation axes.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn typographic_descender(&self) -> Option<i16> {
        self.os_2.map(|table| {
            let v = table.typographic_descender();
            self.apply_metrics_variation(Tag::from_bytes(b"hdsc"), v)
        })
    }

    /// Returns a horizontal typographic face line gap.
    ///
    /// Prefer `Face::line_gap` unless you explicitly want this. This is a more
    /// low-level alternative.
    ///
    /// This method is affected by variation axes.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn typographic_line_gap(&self) -> Option<i16> {
        self.os_2.map(|table| {
            let v = table.typographic_line_gap();
            self.apply_metrics_variation(Tag::from_bytes(b"hlgp"), v)
        })
    }

    /// Returns a vertical face ascender.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn vertical_ascender(&self) -> Option<i16> {
        self.vhea.map(|vhea| vhea.ascender)
            .map(|v| self.apply_metrics_variation(Tag::from_bytes(b"vasc"), v))
    }

    /// Returns a vertical face descender.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn vertical_descender(&self) -> Option<i16> {
        self.vhea.map(|vhea| vhea.descender)
            .map(|v| self.apply_metrics_variation(Tag::from_bytes(b"vdsc"), v))
    }

    /// Returns a vertical face height.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn vertical_height(&self) -> Option<i16> {
        Some(self.vertical_ascender()? - self.vertical_descender()?)
    }

    /// Returns a vertical face line gap.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn vertical_line_gap(&self) -> Option<i16> {
        self.vhea.map(|vhea| vhea.line_gap)
            .map(|v| self.apply_metrics_variation(Tag::from_bytes(b"vlgp"), v))
    }

    /// Returns face's units per EM.
    ///
    /// Guarantee to be in a 16..=16384 range.
    #[inline]
    pub fn units_per_em(&self) -> u16 {
        self.head.units_per_em
    }

    /// Returns face's x height.
    ///
    /// This method is affected by variation axes.
    ///
    /// Returns `None` when OS/2 table is not present or when its version is < 2.
    #[inline]
    pub fn x_height(&self) -> Option<i16> {
        self.os_2.and_then(|os_2| os_2.x_height())
            .map(|v| self.apply_metrics_variation(Tag::from_bytes(b"xhgt"), v))
    }

    /// Returns face's capital height.
    ///
    /// This method is affected by variation axes.
    ///
    /// Returns `None` when OS/2 table is not present or when its version is < 2.
    #[inline]
    pub fn capital_height(&self) -> Option<i16> {
        self.os_2.and_then(|os_2| os_2.capital_height())
            .map(|v| self.apply_metrics_variation(Tag::from_bytes(b"cpht"), v))
    }

    /// Returns face's underline metrics.
    ///
    /// This method is affected by variation axes.
    ///
    /// Returns `None` when `post` table is not present.
    #[inline]
    pub fn underline_metrics(&self) -> Option<LineMetrics> {
        let mut metrics = self.post?.underline_metrics;

        if self.is_variable() {
            self.apply_metrics_variation_to(Tag::from_bytes(b"undo"), &mut metrics.position);
            self.apply_metrics_variation_to(Tag::from_bytes(b"unds"), &mut metrics.thickness);
        }

        Some(metrics)
    }

    /// Returns face's strikeout metrics.
    ///
    /// This method is affected by variation axes.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn strikeout_metrics(&self) -> Option<LineMetrics> {
        let mut metrics = self.os_2?.strikeout_metrics();

        if self.is_variable() {
            self.apply_metrics_variation_to(Tag::from_bytes(b"stro"), &mut metrics.position);
            self.apply_metrics_variation_to(Tag::from_bytes(b"strs"), &mut metrics.thickness);
        }

        Some(metrics)
    }

    /// Returns face's subscript metrics.
    ///
    /// This method is affected by variation axes.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn subscript_metrics(&self) -> Option<ScriptMetrics> {
        let mut metrics = self.os_2?.subscript_metrics();

        if self.is_variable() {
            self.apply_metrics_variation_to(Tag::from_bytes(b"sbxs"), &mut metrics.x_size);
            self.apply_metrics_variation_to(Tag::from_bytes(b"sbys"), &mut metrics.y_size);
            self.apply_metrics_variation_to(Tag::from_bytes(b"sbxo"), &mut metrics.x_offset);
            self.apply_metrics_variation_to(Tag::from_bytes(b"sbyo"), &mut metrics.y_offset);
        }

        Some(metrics)
    }

    /// Returns face's superscript metrics.
    ///
    /// This method is affected by variation axes.
    ///
    /// Returns `None` when OS/2 table is not present.
    #[inline]
    pub fn superscript_metrics(&self) -> Option<ScriptMetrics> {
        let mut metrics = self.os_2?.superscript_metrics();

        if self.is_variable() {
            self.apply_metrics_variation_to(Tag::from_bytes(b"spxs"), &mut metrics.x_size);
            self.apply_metrics_variation_to(Tag::from_bytes(b"spys"), &mut metrics.y_size);
            self.apply_metrics_variation_to(Tag::from_bytes(b"spxo"), &mut metrics.x_offset);
            self.apply_metrics_variation_to(Tag::from_bytes(b"spyo"), &mut metrics.y_offset);
        }

        Some(metrics)
    }

    /// Returns a total number of glyphs in the face.
    ///
    /// Never zero.
    ///
    /// The value was already parsed, so this function doesn't involve any parsing.
    #[inline]
    pub fn number_of_glyphs(&self) -> u16 {
        self.maxp.number_of_glyphs.get()
    }

    /// Returns a list of
    /// [character to glyph index mappings](https://docs.microsoft.com/en-us/typography/opentype/spec/cmap).
    ///
    /// This is a more low-level alternative to [`Face::glyph_index()`](struct.Face.html#method.glyph_index).
    #[inline]
    pub fn character_mapping_subtables(&self) -> cmap::Subtables {
        self.cmap.map(|cmap| cmap.subtables).unwrap_or_default()
    }

    /// Resolves a Glyph ID for a code point.
    ///
    /// Returns `None` instead of `0` when glyph is not found.
    ///
    /// All subtable formats except Mixed Coverage (8) are supported.
    ///
    /// If you need a more low-level control, prefer `Face::character_mapping_subtables`.
    #[inline]
    pub fn glyph_index(&self, code_point: char) -> Option<GlyphId> {
        for encoding in self.character_mapping_subtables() {
            if !encoding.is_unicode() {
                continue;
            }

            if let Some(id) = encoding.glyph_index(code_point) {
                return Some(id);
            }
        }

        None
    }

    /// Resolves a variation of a Glyph ID from two code points.
    ///
    /// Implemented according to
    /// [Unicode Variation Sequences](
    /// https://docs.microsoft.com/en-us/typography/opentype/spec/cmap#format-14-unicode-variation-sequences).
    ///
    /// Returns `None` instead of `0` when glyph is not found.
    #[inline]
    pub fn glyph_variation_index(&self, code_point: char, variation: char) -> Option<GlyphId> {
        for subtable in self.character_mapping_subtables() {
            if let cmap::Format::UnicodeVariationSequences(ref table) = subtable.format {
                return match table.glyph_index(code_point, variation)? {
                    cmap::GlyphVariationResult::Found(v) => Some(v),
                    cmap::GlyphVariationResult::UseDefault => self.glyph_index(code_point),
                };
            }
        }

        None
    }

    /// Returns glyph's horizontal advance.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn glyph_hor_advance(&self, glyph_id: GlyphId) -> Option<u16> {
        #[cfg(feature = "variable-fonts")] {
            let mut advance = self.hmtx?.advance(glyph_id)? as f32;

            if self.is_variable() {
                // Ignore variation offset when `hvar` is not set.
                if let Some(hvar_data) = self.hvar {
                    // We can't use `round()` in `no_std`, so this is the next best thing.
                    advance += hvar::glyph_advance_offset(hvar_data, glyph_id, self.coords())? + 0.5;
                }
            }

            u16::try_num_from(advance)
        }

        #[cfg(not(feature = "variable-fonts"))] {
            self.hmtx?.advance(glyph_id)
        }
    }

    /// Returns glyph's vertical advance.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn glyph_ver_advance(&self, glyph_id: GlyphId) -> Option<u16> {
        #[cfg(feature = "variable-fonts")] {
            let mut advance = self.vmtx?.advance(glyph_id)? as f32;

            if self.is_variable() {
                // Ignore variation offset when `vvar` is not set.
                if let Some(vvar_data) = self.vvar {
                    // We can't use `round()` in `no_std`, so this is the next best thing.
                    advance += hvar::glyph_advance_offset(vvar_data, glyph_id, self.coords())? + 0.5;
                }
            }

            u16::try_num_from(advance)
        }

        #[cfg(not(feature = "variable-fonts"))] {
            self.vmtx?.advance(glyph_id)
        }
    }

    /// Returns glyph's horizontal side bearing.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn glyph_hor_side_bearing(&self, glyph_id: GlyphId) -> Option<i16> {
        #[cfg(feature = "variable-fonts")] {
            let mut bearing = self.hmtx?.side_bearing(glyph_id)? as f32;

            if self.is_variable() {
                // Ignore variation offset when `hvar` is not set.
                if let Some(hvar_data) = self.hvar {
                    // We can't use `round()` in `no_std`, so this is the next best thing.
                    bearing += hvar::glyph_side_bearing_offset(hvar_data, glyph_id, self.coords())? + 0.5;
                }
            }

            i16::try_num_from(bearing)
        }

        #[cfg(not(feature = "variable-fonts"))] {
            self.hmtx?.side_bearing(glyph_id)
        }
    }

    /// Returns glyph's vertical side bearing.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn glyph_ver_side_bearing(&self, glyph_id: GlyphId) -> Option<i16> {
        #[cfg(feature = "variable-fonts")] {
            let mut bearing = self.vmtx?.side_bearing(glyph_id)? as f32;

            if self.is_variable() {
                // Ignore variation offset when `vvar` is not set.
                if let Some(vvar_data) = self.vvar {
                    // We can't use `round()` in `no_std`, so this is the next best thing.
                    bearing += hvar::glyph_side_bearing_offset(vvar_data, glyph_id, self.coords())? + 0.5;
                }
            }

            i16::try_num_from(bearing)
        }

        #[cfg(not(feature = "variable-fonts"))] {
            self.vmtx?.side_bearing(glyph_id)
        }
    }

    /// Returns glyph's vertical origin according to
    /// [Vertical Origin Table](https://docs.microsoft.com/en-us/typography/opentype/spec/vorg).
    pub fn glyph_y_origin(&self, glyph_id: GlyphId) -> Option<i16> {
        self.vorg.map(|vorg| vorg.glyph_y_origin(glyph_id))
    }

    /// Returns glyph's name.
    ///
    /// Uses the `post` and `CFF` tables as sources.
    ///
    /// Returns `None` when no name is associated with a `glyph`.
    #[inline]
    pub fn glyph_name(&self, glyph_id: GlyphId) -> Option<&str> {
        if let Some(name) = self.post.and_then(|post| post.names.get(glyph_id)) {
            return Some(name);
        }

        if let Some(name) = self.cff.as_ref().and_then(|cff1| cff1.glyph_name(glyph_id)) {
            return Some(name);
        }

        None
    }

    /// Returns a [Glyph Definition Table](https://docs.microsoft.com/en-us/typography/opentype/spec/gdef).
    #[cfg(feature = "opentype-layout")]
    #[inline]
    pub fn opentype_definition(&self) -> Option<opentype_layout::DefinitionTable<'a>> {
        self.gdef
    }

    /// Returns a [Glyph Positioning Table](https://docs.microsoft.com/en-us/typography/opentype/spec/gpos).
    #[cfg(feature = "opentype-layout")]
    #[inline]
    pub fn opentype_positioning(&self) -> Option<opentype_layout::LayoutTable<'a>> {
        self.gpos
    }

    /// Returns a [Glyph Substitution Table](https://docs.microsoft.com/en-us/typography/opentype/spec/gsub).
    #[cfg(feature = "opentype-layout")]
    #[inline]
    pub fn opentype_substitution(&self) -> Option<opentype_layout::LayoutTable<'a>> {
        self.gsub
    }

    /// Returns a iterator over kerning subtables.
    ///
    /// Supports both
    /// [OpenType](https://docs.microsoft.com/en-us/typography/opentype/spec/kern)
    /// and
    /// [Apple Advanced Typography](https://developer.apple.com/fonts/TrueType-Reference-Manual/RM06/Chap6kern.html)
    /// variants.
    pub fn kerning_subtables(&self) -> kern::SubtablesIter {
        self.kern.map(|kern| kern.subtables.into_iter()).unwrap_or_default()
    }

    /// Outlines a glyph and returns its tight bounding box.
    ///
    /// **Warning**: since `ttf-parser` is a pull parser,
    /// `OutlineBuilder` will emit segments even when outline is partially malformed.
    /// You must check `outline_glyph()` result before using
    /// `OutlineBuilder`'s output.
    ///
    /// `gvar`, `glyf`, `CFF` and `CFF2` tables are supported.
    /// And they will be accesses in this specific order.
    ///
    /// This method is affected by variation axes.
    ///
    /// Returns `None` when glyph has no outline or on error.
    ///
    /// # Example
    ///
    /// ```
    /// use std::fmt::Write;
    /// use ttf_parser;
    ///
    /// struct Builder(String);
    ///
    /// impl ttf_parser::OutlineBuilder for Builder {
    ///     fn move_to(&mut self, x: f32, y: f32) {
    ///         write!(&mut self.0, "M {} {} ", x, y).unwrap();
    ///     }
    ///
    ///     fn line_to(&mut self, x: f32, y: f32) {
    ///         write!(&mut self.0, "L {} {} ", x, y).unwrap();
    ///     }
    ///
    ///     fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
    ///         write!(&mut self.0, "Q {} {} {} {} ", x1, y1, x, y).unwrap();
    ///     }
    ///
    ///     fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
    ///         write!(&mut self.0, "C {} {} {} {} {} {} ", x1, y1, x2, y2, x, y).unwrap();
    ///     }
    ///
    ///     fn close(&mut self) {
    ///         write!(&mut self.0, "Z ").unwrap();
    ///     }
    /// }
    ///
    /// let data = std::fs::read("tests/fonts/demo.ttf").unwrap();
    /// let face = ttf_parser::Face::from_slice(&data, 0).unwrap();
    /// let mut builder = Builder(String::new());
    /// let bbox = face.outline_glyph(ttf_parser::GlyphId(1), &mut builder).unwrap();
    /// assert_eq!(builder.0, "M 173 267 L 369 267 L 270 587 L 173 267 Z M 6 0 L 224 656 \
    ///                        L 320 656 L 541 0 L 452 0 L 390 200 L 151 200 L 85 0 L 6 0 Z ");
    /// assert_eq!(bbox, ttf_parser::Rect { x_min: 6, y_min: 0, x_max: 541, y_max: 656 });
    /// ```
    #[inline]
    pub fn outline_glyph(
        &self,
        glyph_id: GlyphId,
        builder: &mut dyn OutlineBuilder,
    ) -> Option<Rect> {
        #[cfg(feature = "variable-fonts")] {
            if let Some(ref gvar_table) = self.gvar {
                return gvar::outline(self.glyf?, gvar_table, self.coords(), glyph_id, builder);
            }
        }

        if let Some(table) = self.glyf {
            return table.outline(glyph_id, builder);
        }

        if let Some(ref cff) = self.cff {
            return cff.outline(glyph_id, builder).ok();
        }

        #[cfg(feature = "variable-fonts")] {
            if let Some(ref cff2) = self.cff2 {
                return cff2.outline(self.coords(), glyph_id, builder).ok();
            }
        }

        None
    }

    /// Returns a tight glyph bounding box.
    ///
    /// This is just a shorthand for `outline_glyph()` since only the `glyf` table stores
    /// a bounding box. We ignore `glyf` table bboxes because they can be malformed.
    /// In case of CFF and variable fonts we have to actually outline
    /// a glyph to find it's bounding box.
    ///
    /// When a glyph is defined by a raster or a vector image,
    /// that can be obtained via `glyph_image()`,
    /// the bounding box must be calculated manually and this method will return `None`.
    ///
    /// Note: the returned bbox is not validated in any way. A font file can have a glyph bbox
    /// set to zero/negative width and/or height and this is perfectly ok.
    /// For calculated bboxes, zero width and/or height is also perfectly fine.
    ///
    /// This method is affected by variation axes.
    #[inline]
    pub fn glyph_bounding_box(&self, glyph_id: GlyphId) -> Option<Rect> {
        self.outline_glyph(glyph_id, &mut DummyOutline)
    }

    /// Returns a bounding box that large enough to enclose any glyph from the face.
    #[inline]
    pub fn global_bounding_box(&self) -> Rect {
        self.head.global_bbox
    }

    /// Returns a reference to a glyph's raster image.
    ///
    /// A font can define a glyph using a raster or a vector image instead of a simple outline.
    /// Which is primarily used for emojis. This method should be used to access raster images.
    ///
    /// `pixels_per_em` allows selecting a preferred image size. The chosen size will
    /// be closer to an upper one. So when font has 64px and 96px images and `pixels_per_em`
    /// is set to 72, 96px image will be returned.
    /// To get the largest image simply use `std::u16::MAX`.
    ///
    /// Note that this method will return an encoded image. It should be decoded
    /// by the caller. We don't validate or preprocess it in any way.
    ///
    /// Currently, only PNG images are supported.
    ///
    /// Also, a font can contain both: images and outlines. So when this method returns `None`
    /// you should also try `outline_glyph()` afterwards.
    ///
    /// There are multiple ways an image can be stored in a TrueType font
    /// and this method supports only `sbix`, `CBLC`+`CBDT`.
    /// Font's tables be accesses in this specific order.
    #[inline]
    pub fn glyph_raster_image(&self, glyph_id: GlyphId, pixels_per_em: u16) -> Option<RasterGlyphImage> {
        if let Some(table) = self.sbix {
            if let Some(strike) = table.best_strike(pixels_per_em) {
                return strike.get(glyph_id);
            }
        }

        if let (Some(cblc_data), Some(cbdt_data)) = (self.cblc, self.cbdt) {
            let location = cblc::find_location(cblc_data, glyph_id, pixels_per_em)?;
            return cbdt::parse(cbdt_data, location);
        }

        None
    }

    /// Returns a reference to a glyph's SVG image.
    ///
    /// A font can define a glyph using a raster or a vector image instead of a simple outline.
    /// Which is primarily used for emojis. This method should be used to access SVG images.
    ///
    /// Note that this method will return just an SVG data. It should be rendered
    /// or even decompressed (in case of SVGZ) by the caller.
    /// We don't validate or preprocess it in any way.
    ///
    /// Also, a font can contain both: images and outlines. So when this method returns `None`
    /// you should also try `outline_glyph()` afterwards.
    #[inline]
    pub fn glyph_svg_image(&self, glyph_id: GlyphId) -> Option<&'a [u8]> {
        self.svg.and_then(|svg| svg.documents.find(glyph_id))
    }

    /// Returns an iterator over variation axes.
    #[cfg(feature = "variable-fonts")]
    #[inline]
    pub fn variation_axes(&self) -> VariationAxes {
        self.fvar.map(|fvar| fvar.axes()).unwrap_or_default()
    }

    /// Sets a variation axis coordinate.
    ///
    /// This is the only mutable method in the library.
    /// We can simplify the API a lot by storing the variable coordinates
    /// in the face object itself.
    ///
    /// Since coordinates are stored on the stack, we allow only 32 of them.
    ///
    /// Returns `None` when face is not variable or doesn't have such axis.
    #[cfg(feature = "variable-fonts")]
    pub fn set_variation(&mut self, axis: Tag, value: f32) -> Option<()> {
        if !self.is_variable() {
            return None;
        }

        let v = self.variation_axes().enumerate().find(|(_, a)| a.tag == axis);
        if let Some((idx, a)) = v {
            if idx >= MAX_VAR_COORDS {
                return None;
            }

            self.coordinates.data[idx] = a.normalized_value(value);
        } else {
            return None;
        }

        // TODO: optimize
        if let Some(avar) = self.avar {
            // Ignore error.
            let _ = avar.map_coordinates(self.coordinates.as_mut_slice());
        }

        Some(())
    }

    /// Returns the current normalized variation coordinates.
    #[cfg(feature = "variable-fonts")]
    #[inline]
    pub fn variation_coordinates(&self) -> &[NormalizedCoordinate] {
        self.coordinates.as_slice()
    }

    /// Checks that face has non-default variation coordinates.
    #[cfg(feature = "variable-fonts")]
    #[inline]
    pub fn has_non_default_variation_coordinates(&self) -> bool {
        self.coordinates.as_slice().iter().any(|c| c.0 != 0)
    }

    #[cfg(feature = "variable-fonts")]
    #[inline]
    fn metrics_var_offset(&self, tag: Tag) -> f32 {
        self.mvar.and_then(|table| table.metrics_offset(tag, self.coords())).unwrap_or(0.0)
    }

    #[inline]
    fn apply_metrics_variation(&self, tag: Tag, mut value: i16) -> i16 {
        self.apply_metrics_variation_to(tag, &mut value);
        value
    }


    #[cfg(feature = "variable-fonts")]
    #[inline]
    fn apply_metrics_variation_to(&self, tag: Tag, value: &mut i16) {
        if self.is_variable() {
            let v = f32::from(*value) + self.metrics_var_offset(tag);
            // TODO: Should probably round it, but f32::round is not available in core.
            if let Some(v) = i16::try_num_from(v) {
                *value = v;
            }
        }
    }

    #[cfg(not(feature = "variable-fonts"))]
    #[inline]
    fn apply_metrics_variation_to(&self, _: Tag, _: &mut i16) {
    }

    #[cfg(feature = "variable-fonts")]
    #[inline]
    fn coords(&self) -> &[NormalizedCoordinate] {
        self.coordinates.as_slice()
    }
}

struct DefaultTableProvider<'a> {
    data: &'a [u8],
    tables: LazyArrayIter16<'a, TableRecord>,
}

impl<'a> Iterator for DefaultTableProvider<'a> {
    type Item = Result<(Tag, Option<&'a [u8]>), FaceParsingError>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.tables.next().map(|table| {
            Ok((table.table_tag, {
                let offset = usize::num_from(table.offset);
                let length = usize::num_from(table.length);
                let end = offset.checked_add(length).ok_or(FaceParsingError::MalformedFont)?;
                let range = offset..end;
                self.data.get(range)
            }))
        })
    }
}

impl core::fmt::Debug for Face<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Face()")
    }
}

/// Returns the number of fonts stored in a TrueType font collection.
///
/// Returns `None` if a provided data is not a TrueType font collection.
#[inline]
pub fn fonts_in_collection(data: &[u8]) -> Option<u32> {
    let mut s = Stream::new(data);
    if s.read::<Magic>()? != Magic::FontCollection {
        return None;
    }

    s.skip::<u32>(); // version
    s.read::<u32>()
}
