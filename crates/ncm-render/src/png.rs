use crate::RgbaImage;
use anyhow::Result;
use flate2::{write::ZlibEncoder, Compression};
use std::{io::Write, path::Path};

impl RgbaImage {
    /// Write a lossless RGBA PNG without an image-processing dependency or a native-size raster.
    pub fn write_png(&self, path: impl AsRef<Path>) -> Result<()> {
        self.validate()?;
        let mut file = std::fs::File::create(path)?;
        file.write_all(b"\x89PNG\r\n\x1a\n")?;
        let mut ihdr = Vec::from(self.width.to_be_bytes());
        ihdr.extend(self.height.to_be_bytes());
        ihdr.extend([8, 6, 0, 0, 0]);
        chunk(&mut file, b"IHDR", &ihdr)?;
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::fast());
        let mut filtered = vec![0; self.width as usize * 4 + 1];
        filtered[0] = 1; // Sub filter keeps flat backgrounds and text compact.
        for row in self.pixels.chunks_exact(self.width as usize * 4) {
            for (i, &v) in row.iter().enumerate() {
                filtered[i + 1] = v.wrapping_sub(if i >= 4 { row[i - 4] } else { 0 });
            }
            zlib.write_all(&filtered)?;
        }
        chunk(&mut file, b"IDAT", &zlib.finish()?)?;
        chunk(&mut file, b"IEND", &[])?;
        Ok(())
    }
}
fn chunk(out: &mut impl Write, kind: &[u8; 4], data: &[u8]) -> Result<()> {
    out.write_all(&u32::try_from(data.len())?.to_be_bytes())?;
    out.write_all(kind)?;
    out.write_all(data)?;
    let mut crc = !0u32;
    for byte in kind.iter().chain(data) {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    out.write_all(&(!crc).to_be_bytes())?;
    Ok(())
}
