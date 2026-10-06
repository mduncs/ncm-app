use crate::RgbaImage;
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::{write::ZlibEncoder, Compression};
use ncm_model::{Cell, CellStyle, Grid, Rgb};
use std::io::Write;

/// Verified first 64 entries, in protocol order (tools/kitty_probe.py).
pub const ROW_DIACRITICS: [char; 64] = [
    '\u{0305}', '\u{030d}', '\u{030e}', '\u{0310}', '\u{0312}', '\u{033d}', '\u{033e}', '\u{033f}',
    '\u{0346}', '\u{034a}', '\u{034b}', '\u{034c}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035b}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036a}', '\u{036b}', '\u{036c}', '\u{036d}', '\u{036e}', '\u{036f}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059c}', '\u{059d}', '\u{059e}', '\u{059f}', '\u{05a0}', '\u{05a1}',
    '\u{05a8}', '\u{05a9}', '\u{05ab}', '\u{05ac}', '\u{05af}', '\u{05c4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
];
const PLACEHOLDER: char = '\u{10eeee}';
/// A stacked virtual image and the ordinary terminal rows that address it.
#[derive(Debug, Clone)]
pub struct KittyTile {
    pub image_id: u32,
    pub row_offset: usize,
    pub rows: usize,
    pub transmission: Vec<u8>,
    /// Each row resets SGR, placement underline color and image foreground. No cursor movement.
    pub placeholder_rows: Vec<Vec<u8>>,
}
/// Kitty APC packets written directly to the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyEncoder {
    pub chunk_size: usize,
}
impl Default for KittyEncoder {
    fn default() -> Self {
        Self { chunk_size: 4096 }
    }
}
impl KittyEncoder {
    pub fn encode(&self, image: &RgbaImage, image_id: u32) -> Result<Vec<u8>> {
        self.transmit(image, image_id, None)
    }
    /// Explicit virtual placement dimensions as in the verified probe (rows must be ≤64).
    pub fn encode_placement(
        &self,
        image: &RgbaImage,
        image_id: u32,
        columns: usize,
        rows: usize,
    ) -> Result<Vec<u8>> {
        validate_placement(image_id, columns, rows)?;
        self.transmit(image, image_id, Some((columns, rows)))
    }
    fn transmit(
        &self,
        image: &RgbaImage,
        image_id: u32,
        placement: Option<(usize, usize)>,
    ) -> Result<Vec<u8>> {
        image.validate()?;
        validate_id(image_id)?;
        if self.chunk_size < 4 || self.chunk_size > 4096 || !self.chunk_size.is_multiple_of(4) {
            bail!("invalid kitty chunk size");
        }
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::fast());
        zlib.write_all(&image.pixels)?;
        let encoded = STANDARD.encode(zlib.finish()?);
        let mut output = Vec::new();
        let count = encoded.len().div_ceil(self.chunk_size);
        for (index, chunk) in encoded.as_bytes().chunks(self.chunk_size).enumerate() {
            let more = u8::from(index + 1 < count);
            let header = if index == 0 {
                let placement = placement
                    .map(|(c, r)| format!(",c={c},r={r}"))
                    .unwrap_or_default();
                format!(
                    "a=T,f=32,o=z,i={image_id},s={},v={},U=1,q=2{placement},m={more}",
                    image.width, image.height
                )
            } else {
                format!("m={more},q=2")
            };
            let mut packet = format!("\x1b_G{header};").into_bytes();
            packet.extend_from_slice(chunk);
            packet.extend_from_slice(b"\x1b\\");
            output.extend(packet);
        }
        Ok(output)
    }
    pub fn query(&self, image_id: u32) -> Vec<u8> {
        format!("\x1b_Ga=q,i={image_id},t=d,s=1,v=1,f=32;AAAA/w==\x1b\\").into_bytes()
    }
    pub fn delete(&self, image_id: u32) -> Result<Vec<u8>> {
        validate_id(image_id)?;
        Ok(format!("\x1b_Ga=d,d=I,i={image_id},q=2;\x1b\\").into_bytes())
    }
    /// RGB model representation for existing consumers. Prefer `placeholder_rows` for indexed ids.
    pub fn placeholders(&self, image_id: u32, columns: usize, rows: usize) -> Result<Grid> {
        validate_placement(image_id, columns, rows)?;
        let style = CellStyle {
            fg: Some(Rgb::new(
                (image_id >> 16) as u8,
                (image_id >> 8) as u8,
                image_id as u8,
            )),
            ..CellStyle::default()
        };
        let mut grid = Grid::new(
            columns,
            rows,
            Cell {
                text: PLACEHOLDER.into(),
                width: 1,
                style,
            },
        )?;
        for (row, diacritic) in ROW_DIACRITICS.iter().take(rows).enumerate() {
            grid.get_mut(0, row)
                .context("placeholder first column")?
                .text = format!("{PLACEHOLDER}{diacritic}{}", ROW_DIACRITICS[0]);
        }
        Ok(grid)
    }
    /// First cell encodes row + column 0, remaining columns are inferred from their left neighbor.
    /// These bytes are text, never wrapped in tmux passthrough.
    pub fn placeholder_rows(
        &self,
        image_id: u32,
        columns: usize,
        rows: usize,
    ) -> Result<Vec<Vec<u8>>> {
        validate_placement(image_id, columns, rows)?;
        (0..rows)
            .map(|row| self.placeholder_row(image_id, row, 0, columns))
            .collect()
    }
    /// A restored fragment can start at a column in the table. For columns ≥64 restore from col 0.
    pub fn placeholder_row(
        &self,
        image_id: u32,
        row: usize,
        start_column: usize,
        count: usize,
    ) -> Result<Vec<u8>> {
        validate_id(image_id)?;
        if row >= 64 || start_column >= 64 || count == 0 {
            bail!("placeholder anchor outside 64-entry table or empty fragment");
        }
        let color = if image_id <= 255 {
            format!("\x1b[38;5;{image_id}m")
        } else {
            format!(
                "\x1b[38;2;{};{};{}m",
                image_id >> 16,
                (image_id >> 8) & 255,
                image_id & 255
            )
        };
        let mut row_bytes = format!(
            "\x1b[0m\x1b[58;2;0;0;0m{color}{PLACEHOLDER}{}{}",
            ROW_DIACRITICS[row], ROW_DIACRITICS[start_column]
        )
        .into_bytes();
        for _ in 1..count {
            row_bytes.extend_from_slice("\u{10eeee}".as_bytes());
        }
        row_bytes.extend_from_slice(b"\x1b[0m");
        Ok(row_bytes)
    }
    /// Split above 64 terminal rows. Caller reserves consecutive ids and keeps them stable on updates.
    /// Integer pixel boundaries partition all source scanlines exactly once.
    pub fn encode_tiled(
        &self,
        image: &RgbaImage,
        first_id: u32,
        columns: usize,
        rows: usize,
    ) -> Result<Vec<KittyTile>> {
        image.validate()?;
        if columns == 0 || rows == 0 || rows > image.height as usize {
            bail!("invalid tiled placement geometry");
        }
        let count = rows.div_ceil(64);
        let last_id = first_id
            .checked_add(u32::try_from(count - 1)?)
            .context("image ids overflow")?;
        validate_id(first_id)?;
        validate_id(last_id)?;
        let mut tiles = Vec::with_capacity(count);
        for offset in (0..rows).step_by(64) {
            let tile_rows = (rows - offset).min(64);
            let top = offset as u64 * image.height as u64 / rows as u64;
            let bottom = (offset + tile_rows) as u64 * image.height as u64 / rows as u64;
            let stride = image.width as usize * 4;
            let tile = RgbaImage {
                width: image.width,
                height: (bottom - top) as u32,
                pixels: image.pixels[top as usize * stride..bottom as usize * stride].to_vec(),
            };
            let image_id = first_id + (offset / 64) as u32;
            tiles.push(KittyTile {
                image_id,
                row_offset: offset,
                rows: tile_rows,
                transmission: self.encode_placement(&tile, image_id, columns, tile_rows)?,
                placeholder_rows: self.placeholder_rows(image_id, columns, tile_rows)?,
            });
        }
        Ok(tiles)
    }
}
fn validate_id(id: u32) -> Result<()> {
    if id == 0 || id >= 1 << 24 {
        bail!("kitty image id must fit nonzero 24-bit foreground");
    }
    Ok(())
}
fn validate_placement(id: u32, columns: usize, rows: usize) -> Result<()> {
    validate_id(id)?;
    if columns == 0 || rows == 0 || rows > 64 {
        bail!("virtual placement needs columns>0 and 1–64 rows; use encode_tiled above 64");
    }
    Ok(())
}
