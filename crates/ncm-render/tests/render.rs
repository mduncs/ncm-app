use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::read::ZlibDecoder;
use ncm_model::{Cell, CellStyle, Grid, Rgb};
use ncm_render::{braille_thumbnail, KittyEncoder, RgbaImage, ROW_DIACRITICS};
use std::io::Read;

fn decode(bytes: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(bytes)?;
    let encoded = text
        .split("\x1b_G")
        .skip(1)
        .map(|p| p.split_once(';').unwrap().1.trim_end_matches("\x1b\\"))
        .collect::<String>();
    let compressed = STANDARD.decode(encoded)?;
    let mut raw = Vec::new();
    ZlibDecoder::new(compressed.as_slice()).read_to_end(&mut raw)?;
    Ok(raw)
}
#[test]
fn chunk_roundtrip_uses_direct_apc() -> Result<()> {
    let image = RgbaImage {
        width: 2,
        height: 1,
        pixels: vec![1, 2, 3, 255, 4, 5, 6, 255],
    };
    let encoder = KittyEncoder { chunk_size: 4 };
    let raw = encoder.encode(&image, 42)?;
    assert_eq!(decode(&raw)?, image.pixels);
    let text = std::str::from_utf8(&raw)?;
    assert!(text.contains("a=T,f=32,o=z,i=42,s=2,v=1,U=1,q=2,m=1"));
    assert!(text.contains("m=0,q=2;"));
    assert!(!raw.windows(7).any(|p| p == b"\x1bPtmux;"));
    assert!(std::str::from_utf8(&encoder.delete(42)?)?.contains("a=d,d=I,i=42,q=2"));
    assert!(std::str::from_utf8(&encoder.query(42))?.contains("a=q,i=42,t=d,s=1,v=1,f=32;AAAA/w=="));
    Ok(())
}
#[test]
fn inferred_columns_and_tall_card_partition() -> Result<()> {
    let encoder = KittyEncoder::default();
    let placeholders = encoder.placeholders(7, 130, 64)?;
    assert_eq!(
        placeholders.get(0, 63).unwrap().text,
        format!("\u{10eeee}{}{}", ROW_DIACRITICS[63], ROW_DIACRITICS[0])
    );
    assert_eq!(placeholders.get(129, 63).unwrap().text, "\u{10eeee}");
    let rows = encoder.placeholder_rows(7, 130, 64)?;
    assert!(std::str::from_utf8(&rows[0])?.contains("38;5;7m"));
    assert_eq!(
        std::str::from_utf8(&rows[0])?.matches('\u{10eeee}').count(),
        130
    );
    assert!(std::str::from_utf8(&encoder.placeholder_rows(256, 1, 1)?[0])?.contains("38;2;0;1;0m"));
    assert!(encoder.placeholders(7, 5, 65).is_err());
    assert!(encoder.placeholders(0, 5, 5).is_err());
    let image = RgbaImage {
        width: 2,
        height: 211,
        pixels: (0..211)
            .flat_map(|y| [y as u8, 0, 0, 255].repeat(2))
            .collect(),
    };
    let tiles = encoder.encode_tiled(&image, 8, 130, 130)?;
    assert_eq!(
        tiles
            .iter()
            .map(|t| (t.image_id, t.row_offset, t.rows))
            .collect::<Vec<_>>(),
        [(8, 0, 64), (9, 64, 64), (10, 128, 2)]
    );
    let decoded = tiles
        .iter()
        .map(|t| decode(&t.transmission))
        .collect::<Result<Vec<_>>>()?
        .concat();
    assert_eq!(decoded, image.pixels);
    assert!(std::str::from_utf8(&tiles[0].transmission)?.contains("c=130,r=64"));
    Ok(())
}
#[test]
fn braille_dot_positions_and_color_majority() -> Result<()> {
    let red = Rgb::new(220, 20, 30);
    let blue = Rgb::new(1, 36, 86);
    let mut grid = Grid::new(
        2,
        2,
        Cell::blank(CellStyle {
            bg: Some(blue),
            ..CellStyle::default()
        }),
    )?;
    *grid.get_mut(0, 0).unwrap() = Cell {
        text: "x".into(),
        width: 1,
        style: CellStyle {
            fg: Some(red),
            bg: Some(blue),
            ..CellStyle::default()
        },
    };
    let out = braille_thumbnail(&grid, 1, 1)?;
    assert_eq!(out.get(0, 0).unwrap().text, "\u{2803}");
    assert_eq!(out.get(0, 0).unwrap().style.fg, Some(red));
    assert_eq!(out.get(0, 0).unwrap().style.bg, Some(blue));
    grid.get_mut(0, 0).unwrap().text = " ".into();
    assert_eq!(braille_thumbnail(&grid, 1, 1)?.get(0, 0).unwrap().text, " ");
    Ok(())
}

#[test]
fn braille_reverse_resolves_default_foreground() -> Result<()> {
    let tint = Rgb::new(1, 36, 86);
    let grid = Grid::new(
        2,
        2,
        Cell {
            text: "x".into(),
            width: 1,
            style: CellStyle {
                bg: Some(tint),
                reverse: true,
                ..CellStyle::default()
            },
        },
    )?;
    let out = braille_thumbnail(&grid, 1, 1)?;
    assert_eq!(out.get(0, 0).unwrap().style.fg, Some(tint));
    assert_eq!(
        out.get(0, 0).unwrap().style.bg,
        Some(ncm_render::DEFAULT_FOREGROUND)
    );
    Ok(())
}

#[cfg(target_os = "macos")]
mod raster {
    use super::*;
    use ncm_model::{AgentState, Cursor, PaneInfo};
    use ncm_render::{
        Overlays, PaneTint, ThumbnailRenderer, ThumbnailSize, TintResolver, HAIRLINE,
        PAGE_BACKGROUND, POWERSHELL_TINT, WSL_TINT,
    };
    fn create_renderer() -> Result<ThumbnailRenderer> {
        ThumbnailRenderer::with_font(&std::fs::read("/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/SF-Mono-Regular.otf")?)
    }
    fn pixel(image: &RgbaImage, x: usize, y: usize) -> Rgb {
        let i = (y * image.width as usize + x) * 4;
        Rgb::new(image.pixels[i], image.pixels[i + 1], image.pixels[i + 2])
    }
    #[test]
    fn picture_inset_cover_crop_and_layers() -> Result<()> {
        let mut renderer = create_renderer()?;
        let mut grid = Grid::new(10, 8, Cell::default())?;
        for y in 0..8 {
            for x in 0..10 {
                grid.get_mut(x, y).unwrap().style.bg =
                    Some(Rgb::new(x as u8 * 20, y as u8 * 20, 60));
            }
        }
        let size = ThumbnailSize {
            width: 88,
            height: 40,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let mut overlays = Overlays::default();
        let image = renderer.render(&grid, &[], &overlays, size)?;
        assert_eq!(pixel(&image, 0, 0), PAGE_BACKGROUND);
        assert_eq!(pixel(&image, 3, 20), PAGE_BACKGROUND);
        assert_eq!(pixel(&image, 4, 20), HAIRLINE);
        assert_eq!(pixel(&image, 6, 6), Rgb::new(0, 120, 60)); // top cropped: source rows 6 and 7 only
        assert_eq!(pixel(&image, 6, 33), Rgb::new(0, 140, 60));
        overlays.state = AgentState::Needs;
        overlays.selected = true;
        let selected = renderer.render(&grid, &[], &overlays, size)?;
        assert_eq!(pixel(&selected, 1, 20), PAGE_BACKGROUND);
        overlays.selected = false;
        assert_eq!(renderer.render(&grid, &[], &overlays, size)?, selected);
        assert_eq!(pixel(&selected, 5, 20), AgentState::Needs.color().unwrap());
        overlays.state = AgentState::None;
        overlays.selected = false;
        assert_eq!(renderer.render(&grid, &[], &overlays, size)?, image);
        // Horizontal centering for a tall card crops both sides equally.
        let tall = ThumbnailSize {
            width: 40,
            height: 136,
            ..size
        };
        let image = renderer.render(&grid, &[], &overlays, tall)?;
        assert_eq!(pixel(&image, 6, 8), Rgb::new(60, 0, 60));
        Ok(())
    }
    #[test]
    fn preview_fits_whole_window_and_invalidates_cover_cache() -> Result<()> {
        let mut renderer = create_renderer()?;
        let mut grid = Grid::new(10, 8, Cell::default())?;
        for y in 0..8 {
            for x in 0..10 {
                grid.get_mut(x, y).unwrap().style.bg =
                    Some(Rgb::new(x as u8 * 20, y as u8 * 20, 60));
            }
        }
        let size = ThumbnailSize {
            width: 88,
            height: 40,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let overlays = Overlays::default();
        let cover = renderer
            .render_card("window", &grid, &[], &overlays, size)?
            .clone();
        let fit = renderer.render_preview("window", &grid, &[], &overlays, size)?;
        assert_ne!(fit, &cover);
        assert_eq!(pixel(fit, 35, 6), Rgb::new(0, 0, 60));
        assert_eq!(pixel(fit, 53, 33), Rgb::new(180, 140, 60));
        assert_eq!(pixel(fit, 10, 20), PAGE_BACKGROUND);
        assert_eq!(
            renderer.render_card("window", &grid, &[], &overlays, size)?,
            &cover
        );
        Ok(())
    }
    #[test]
    fn incremental_matches_fresh_with_fractional_rows_and_text_overlays() -> Result<()> {
        let mut renderer = create_renderer()?;
        let mut fresh = create_renderer()?;
        let mut grid = Grid::new(35, 12, Cell::default())?;
        for y in 0..12 {
            for x in 0..35 {
                *grid.get_mut(x, y).unwrap() = Cell {
                    text: if x % 5 == 0 {
                        "e\u{301}".into()
                    } else {
                        "M".into()
                    },
                    width: 1,
                    style: CellStyle {
                        fg: Some(Rgb::new(220, 223, 228)),
                        bg: if x % 7 == 0 {
                            Some(Rgb::new(25, 55, 70))
                        } else {
                            None
                        },
                        bold: x % 3 == 0,
                        italic: x % 4 == 0,
                        reverse: x % 11 == 0,
                        underline: y % 3 == 0,
                        ..CellStyle::default()
                    },
                };
            }
        }
        let tint = vec![PaneTint {
            pane_id: "%1".into(),
            x: 0,
            y: 0,
            width: 35,
            height: 12,
            color: Rgb::new(32, 18, 29),
        }];
        let size = ThumbnailSize {
            width: 153,
            height: 73,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let overlays = Overlays::default();
        let initial = renderer
            .render_card("a", &grid, &tint, &overlays, size)?
            .clone();
        assert!(initial.pixels.chunks_exact(4).any(|p| p[0] > 80)); // actual glyph ink
        grid.get_mut(14, 10).unwrap().text = "*".into();
        grid.get_mut(0, 2).unwrap().style.bg = Some(Rgb::new(12, 64, 31));
        let updated = renderer
            .render_card("a", &grid, &tint, &overlays, size)?
            .clone();
        assert!(!renderer.last_stats().full_render);
        assert_eq!(renderer.last_stats().changed_rows, 2);
        assert_eq!(updated, fresh.render(&grid, &tint, &overlays, size)?);
        let new_text = Overlays {
            title: "NO IMAGE OVERLAYS".into(),
            age: "5m".into(),
            unanswered: Some("unanswered 3m".into()),
            ..overlays.clone()
        };
        assert_eq!(
            *renderer.render_card("a", &grid, &tint, &new_text, size)?,
            updated
        );
        assert_eq!(renderer.last_stats().rasterized_rows, 0);
        renderer.render_card("b", &grid, &tint, &overlays, size)?;
        renderer.render_card("a", &grid, &tint, &overlays, size)?;
        assert_eq!(renderer.last_stats().changed_rows, 0);
        let resized = ThumbnailSize { width: 161, ..size };
        renderer.render_card("a", &grid, &tint, &overlays, resized)?;
        assert!(renderer.last_stats().full_render);
        Ok(())
    }
    #[test]
    fn border_color_updates_match_fresh_without_rasterizing_unchanged_rows() -> Result<()> {
        let mut renderer = create_renderer()?;
        let mut grid = Grid::new(20, 8, Cell::default())?;
        grid.get_mut(0, 2).unwrap().text = "x".into();
        let size = ThumbnailSize {
            width: 160,
            height: 128,
            cell_width: 8.,
            cell_height: 16.,
        };
        renderer.render_card("a", &grid, &[], &Overlays::default(), size)?;
        for color in [
            Some(Rgb::new(180, 60, 80)),
            Some(Rgb::new(80, 180, 60)),
            None,
        ] {
            let overlays = Overlays {
                edge_color: color,
                ..Default::default()
            };
            let updated = renderer
                .render_card("a", &grid, &[], &overlays, size)?
                .clone();
            if color.is_some() {
                assert_eq!(renderer.last_stats().rasterized_rows, 0);
            }
            assert_eq!(
                updated,
                create_renderer()?.render(&grid, &[], &overlays, size)?
            );
        }
        Ok(())
    }
    #[test]
    fn tint_priority_and_project_default() {
        let mut pane = PaneInfo {
            id: "%1".into(),
            index: 0,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            active: true,
            pid: 0,
            cwd: "/code/project".into(),
            title: "PowerShell".into(),
            cursor: Cursor::default(),
            tint: None,
            harness: None,
        };
        let resolver = TintResolver::default();
        assert_eq!(resolver.resolve(&pane), POWERSHELL_TINT);
        pane.title = "WSL Ubuntu".into();
        assert_eq!(resolver.resolve(&pane), WSL_TINT);
        pane.tint = Some(Rgb::new(5, 6, 7));
        assert_eq!(resolver.resolve(&pane), Rgb::new(5, 6, 7));
        pane.tint = None;
        pane.title.clear();
        let project = resolver.resolve(&pane);
        assert!(project.r < 40 && project.g < 40 && project.b < 40);
    }
}
