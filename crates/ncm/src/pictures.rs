//! Bounded off-thread rasterization and compression; the UI only writes prepared packets.
use anyhow::Result;
use ncm_model::{Grid, WindowInfo};
use ncm_render::{
    KittyEncoder, Overlays, RgbaImage, ThumbnailRenderer, ThumbnailSize, TintResolver,
};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
};

pub struct Request {
    pub key: String,
    pub generation: u64,
    pub layout_epoch: u64,
    pub grid: Arc<Grid>,
    pub window: WindowInfo,
    pub size: ThumbnailSize,
    pub cells: (usize, usize),
    pub ids: Vec<u32>,
    pub tile_rows: usize,
    pub quick: bool,
}
pub struct Picture {
    pub key: String,
    pub generation: u64,
    pub bytes: Result<Vec<u8>>,
}
pub struct Pictures {
    pub requests: SyncSender<Request>,
    pub results: Receiver<Picture>,
}
impl Pictures {
    pub fn start(log: Option<&std::path::Path>) -> Result<Self> {
        let (requests, rx) = mpsc::sync_channel::<Request>(1);
        let (tx, results) = mpsc::sync_channel(1);
        let mut metrics = crate::metrics::Metrics::new(false, log)?;
        std::thread::Builder::new()
            .name("ncm-pictures".into())
            .spawn(move || {
                #[cfg(test)]
                crate::panic_log::inject("ncm-pictures");
                let mut renderer = ThumbnailRenderer::default();
                let mut hashes = HashMap::new();
                let mut epoch = 0;
                let mut generation = 0;
                while let Ok(r) = rx.recv() {
                    if epoch != r.layout_epoch {
                        renderer.clear_cards();
                        epoch = r.layout_epoch;
                    }
                    if generation != r.generation {
                        hashes.clear();
                        generation = r.generation;
                    }
                    let started = std::time::Instant::now();
                    metrics.trace(
                        "picture_start",
                        &format!("key={} generation={}", r.key, r.generation),
                    );
                    let bytes = prepare(&mut renderer, &mut hashes, &r);
                    metrics.trace(
                        "picture_ready",
                        &format!(
                            "key={} generation={} duration_ms={:.3}",
                            r.key,
                            r.generation,
                            started.elapsed().as_secs_f64() * 1000.
                        ),
                    );
                    if tx
                        .send(Picture {
                            key: r.key,
                            generation: r.generation,
                            bytes,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self { requests, results })
    }
}
fn prepare(
    renderer: &mut ThumbnailRenderer,
    hashes: &mut HashMap<u32, u64>,
    r: &Request,
) -> Result<Vec<u8>> {
    let tints = TintResolver::new(std::slice::from_ref(&r.window)).window_tints(&r.window);
    let overlays = if r.quick {
        Overlays::default()
    } else {
        Overlays {
            state: r.window.agent_state,
            edge_color: crate::view::window_edge(&r.window),
            ..Default::default()
        }
    };
    let image = if r.quick {
        renderer.render_preview(&r.key, &r.grid, &tints, &overlays, r.size)?
    } else {
        renderer.render_card(&r.key, &r.grid, &tints, &overlays, r.size)?
    };
    let encoder = KittyEncoder::default();
    let rows = r.tile_rows;
    let px_height = image.height as usize / r.cells.1;
    let stride = image.width as usize * 4;
    let mut bytes = Vec::new();
    for (part, &id) in r.ids.iter().enumerate() {
        let top = part * rows * px_height;
        let height = (image.height as usize - top).min(rows * px_height);
        let pixels = &image.pixels[top * stride..(top + height) * stride];
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        pixels.hash(&mut hash);
        let hash = hash.finish();
        if hashes.get(&id) == Some(&hash) {
            continue;
        }
        hashes.insert(id, hash);
        bytes.extend(encoder.encode_placement(
            &RgbaImage {
                width: image.width,
                height: height as u32,
                pixels: pixels.to_vec(),
            },
            id,
            r.cells.0,
            (r.cells.1 - part * rows).min(rows),
        )?);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_grid_strips_are_not_retransmitted() -> Result<()> {
        let mut renderer = ThumbnailRenderer::default();
        let mut hashes = HashMap::new();
        let mut request = Request {
            key: "card".into(),
            generation: 0,
            layout_epoch: 0,
            grid: Arc::new(Grid::new(20, 8, ncm_model::Cell::default())?),
            window: crate::demo::windows().remove(0),
            size: ThumbnailSize {
                width: 160,
                height: 128,
                cell_width: 8.,
                cell_height: 16.,
            },
            cells: (20, 8),
            ids: vec![1, 2],
            tile_rows: 4,
            quick: false,
        };
        assert!(!prepare(&mut renderer, &mut hashes, &request)?.is_empty());
        assert!(prepare(&mut renderer, &mut hashes, &request)?.is_empty());
        Arc::make_mut(&mut request.grid).get_mut(1, 0).unwrap().text = "x".into();
        let changed = String::from_utf8(prepare(&mut renderer, &mut hashes, &request)?)?;
        assert!(changed.contains("i=1,"));
        assert!(!changed.contains("i=2,"));
        Ok(())
    }
}
