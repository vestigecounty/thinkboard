//! Text bitmaps and semantic-zoom clusters — the two caches the board draws
//! from, both recomputed off the main thread.
//!
//! Every text node is drawn as a bitmap rasterized at a reference zoom and
//! GPU-scaled from there, so gestures never touch the glyph rasterizer. A
//! bitmap is only re-rasterized when the settled zoom drifts outside its
//! quality band or its text changes; a fresh one is warmed invisibly and
//! cross-faded in, because iced uploads large images on a worker thread and
//! draws nothing until that finishes.
//!
//! **Each batch is for one board.** Both are tagged with the board they were
//! started for (`App::board_epoch`), and a result landing after that board
//! was left is dropped: it used to be installed on the next one — its
//! clusters' titles over the wrong cards, bitmaps keyed by index on cards
//! they weren't made from — and to hold off the new board's own batch until
//! it had.
//!
//! **A bitmap knows what it's of** — the text it was made from and the size
//! it was laid out at (`Snapshot::depicts`). One for a card since changed is
//! turned away as its batch lands, let go of when a refresh finds the card
//! resized, and never drawn on a card of another size. Saying so at every
//! change wasn't enough: an answer's cards were widened to one width without
//! it, and a bitmap made partway through the answer was drawn stretched over
//! the wider card, its lines still wrapped at the old width.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::Task;
use iced::widget::image;

use super::{App, Message};
use crate::canvas::text_raster;
use crate::document::cluster;
use crate::model::CanvasNode;
use crate::theme::Theme as ColorTheme;

/// How long a freshly rasterized text bitmap is drawn invisibly (warming
/// the GPU upload) before replacing the old one. iced uploads images larger
/// than 2 MB on a worker thread and draws nothing until that finishes, so
/// swapping handles immediately would blank the node for a few frames.
const SNAPSHOT_WARM_UP: Duration = Duration::from_millis(300);

/// After warm-up, a re-rasterized snapshot cross-fades in over this window
/// (drawn on top of the outgoing one) before being promoted, so a sharper
/// re-raster eases in instead of popping.
const SNAPSHOT_FADE: Duration = Duration::from_millis(140);

/// The size past which iced_wgpu uploads an image on its worker thread rather
/// than in the frame that draws it (`MAX_SYNC_SIZE` in
/// `vendor/iced_wgpu/src/image/cache.rs`). A bitmap this big draws as nothing
/// until its upload lands — which is what warming one is for.
const ASYNC_UPLOAD_BYTES: usize = 2 * 1024 * 1024;

pub struct StagedSnapshot {
    pub snapshot: text_raster::Snapshot,
    since: Instant,
    /// Cross-fade opacity: 0 while the texture warms, then ramps to 1 over the
    /// fade window, at which point the snapshot is promoted.
    pub opacity: f32,
}

/// Bitmap renditions of the text nodes — the only way text is drawn on the
/// board — keyed by node index.
#[derive(Default)]
pub(super) struct Rasters {
    /// The bitmaps currently on screen.
    pub(super) text: HashMap<usize, text_raster::Snapshot>,
    /// Freshly rasterized bitmaps being upload-warmed before they replace the
    /// entries in `text`.
    pub(super) staging: HashMap<usize, StagedSnapshot>,
    /// A background rasterization is in flight; acts as a debounce.
    pending: bool,
}

impl Rasters {
    /// The bitmaps on screen, taken away with the board they were made for —
    /// and no batch waiting, since any still being made is for that board.
    pub(super) fn take(&mut self) -> HashMap<usize, text_raster::Snapshot> {
        self.staging.clear();
        self.pending = false;
        std::mem::take(&mut self.text)
    }

    /// A board's bitmaps, back with it. One over the in-frame upload limit
    /// is staged, as a fresh one is: the GPU let go of it when the board was
    /// left, and installed straight away it would draw as nothing until
    /// uploaded again.
    pub(super) fn restore(&mut self, bitmaps: HashMap<usize, text_raster::Snapshot>) {
        let now = Instant::now();
        for (index, snapshot) in bitmaps {
            if snapshot.bytes >= ASYNC_UPLOAD_BYTES {
                self.staging.insert(index, StagedSnapshot { snapshot, since: now, opacity: 0.0 });
            } else {
                self.text.insert(index, snapshot);
            }
        }
    }

    /// Drops every bitmap (e.g. when the document is swapped and the node
    /// indices they're keyed by no longer mean anything).
    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.staging.clear();
    }

    /// Whether any bitmap is mid warm-up or cross-fade (so frames must keep
    /// coming until it settles).
    pub(super) fn staging_any(&self) -> bool {
        !self.staging.is_empty()
    }

    /// Drops one node's bitmap so it draws live until a fresh raster arrives.
    /// [`App::refresh_snapshots`] keeps a snapshot while the zoom stays in band
    /// — it can't see that the text or size changed — so any text change
    /// outside the edit-commit path (AI streaming, the pending "…" dots) must
    /// invalidate it explicitly, or the stale bitmap keeps showing.
    pub(super) fn invalidate(&mut self, index: usize) {
        self.text.remove(&index);
        self.staging.remove(&index);
    }
}

/// The semantic-zoom cluster summaries, recomputed in the background after
/// geometry/edge changes.
#[derive(Default)]
pub(super) struct Clusters {
    pub(super) list: Vec<cluster::Cluster>,
    /// Arrows between connected clusters, shown in cluster mode.
    pub(super) links: Vec<cluster::ClusterLink>,
    /// Cluster label bitmaps, keyed by cluster index — same rendering path as
    /// node labels. Rasterized once at the reference zoom.
    pub(super) snapshots: HashMap<usize, text_raster::Snapshot>,
    /// Title bitmaps not yet on the GPU. A title is drawn live until its
    /// bitmap's upload has finished, rather than as nothing: one this large
    /// is uploaded off the frame that first draws it, and the titles went
    /// blank for a moment after every recluster and every switch of board.
    waiting: HashMap<usize, text_raster::Snapshot>,
    /// Counts the clusterings installed, so a title uploaded for one that's
    /// since been replaced isn't put on another's cluster.
    generation: u64,
    pending: bool,
    /// A change arrived while a recluster was in flight; recompute again.
    dirty: bool,
    /// The groups a cluster drag carries, by the cluster it's dragging:
    /// decided as it starts (`App::move_cluster`), or a cluster would pick up
    /// an empty group it was dragged across.
    pub(super) carried: Option<(usize, Vec<usize>)>,
}

impl Clusters {
    /// Drops the title bitmaps — made in colours or a face that no longer
    /// stand — and any on their way to the GPU.
    pub(super) fn drop_titles(&mut self) {
        self.snapshots.clear();
        self.waiting.clear();
        self.generation += 1;
    }

    /// Every title bitmap made for these clusters, on the GPU or not.
    pub(super) fn into_titles(self) -> (Clusters, HashMap<usize, text_raster::Snapshot>) {
        let Clusters { list, links, mut snapshots, waiting, generation, .. } = self;
        snapshots.extend(waiting);
        (Clusters { list, links, generation, ..Clusters::default() }, snapshots)
    }
}

/// Rasterizes each text cluster's label into a bitmap (same Markdown path as
/// node labels). Cluster boxes are large and the font is fixed, so one raster
/// at the reference zoom stays crisp for the whole cluster-visible range.
/// Returns nothing in live-text mode, where labels are drawn from Markdown.
fn cluster_snapshots(
    clusters: &[cluster::Cluster],
    reference_zoom: f32,
    theme: ColorTheme,
    center: bool,
    rasterize: bool,
) -> HashMap<usize, text_raster::Snapshot> {
    if !rasterize {
        return HashMap::new();
    }
    clusters
        .iter()
        .enumerate()
        .filter(|(_, c)| c.image_id.is_none() && !c.text.is_empty())
        .filter_map(|(index, c)| {
            let snapshot = text_raster::snapshot_markdown(
                &c.text,
                c.bounds.width,
                c.bounds.height,
                (c.bounds.width - cluster::PADDING * 2.0).max(20.0),
                c.font_size,
                theme,
                center,
                reference_zoom,
            )?;
            Some((index, snapshot))
        })
        .collect()
}

/// The text a card's bitmap is made from.
fn bitmap_text(node: &CanvasNode) -> &str {
    node.text.as_deref().unwrap_or("(no content)")
}

/// Whether `snapshot` is `node` as it is now.
fn depicts(snapshot: &text_raster::Snapshot, node: &CanvasNode) -> bool {
    snapshot.depicts(bitmap_text(node), node.width, node.height)
}

impl App {
    /// The initial clustering, computed synchronously at startup (there is no
    /// board on screen yet to keep responsive).
    pub(super) fn initial_clusters(&self) -> Clusters {
        let clustering = cluster::compute(&self.nodes, &self.edges, &self.theme);
        let snapshots = cluster_snapshots(
            &clustering.clusters,
            self.config.cluster_zoom,
            self.theme,
            self.config.center_card_text,
            self.config.rasterize_text,
        );
        Clusters {
            list: clustering.clusters,
            links: clustering.links,
            snapshots,
            ..Clusters::default()
        }
    }

    /// The clusters alone, computed now, their titles drawn live until a
    /// background pass brings their bitmaps — for a board opened at a zoom
    /// that shows clusters, which would otherwise draw every card until
    /// that pass had finished.
    pub(super) fn clusters_now(&mut self) -> Task<Message> {
        let clustering = cluster::compute(&self.nodes, &self.edges, &self.theme);
        self.clusters = Clusters { list: clustering.clusters, links: clustering.links, ..Clusters::default() };
        // Again, for the titles: the fitting is cached, so it's cheap twice.
        self.schedule_recluster()
    }

    /// Installs title bitmaps for the clusters on screen, each once it's on
    /// the GPU — until then the title is drawn live. In live-text mode there
    /// are none, and nothing to wait for.
    pub(super) fn install_titles(&mut self, titles: HashMap<usize, text_raster::Snapshot>) -> Task<Message> {
        self.clusters.generation += 1;
        self.clusters.snapshots.clear();
        let (epoch, generation) = (self.board_epoch, self.clusters.generation);
        let uploads: Vec<Task<Message>> = titles
            .iter()
            .map(|(&index, title)| {
                image::allocate(title.handle.clone()).map(move |allocation| Message::ClusterTitleUploaded {
                    epoch,
                    generation,
                    index,
                    allocation: allocation.ok(),
                })
            })
            .collect();
        self.clusters.waiting = titles;
        self.cache.clear();
        Task::batch(uploads)
    }

    /// A title's bitmap is on the GPU, and takes over from its live text.
    /// The allocation is let go once it has: drawn every frame the clusters
    /// show, the bitmap stays, and pinned it would hold GPU memory for every
    /// cluster's title while zoomed in on the cards.
    pub(super) fn title_uploaded(&mut self, epoch: u64, generation: u64, index: usize, allocation: Option<image::Allocation>) {
        if epoch != self.board_epoch || generation != self.clusters.generation {
            return;
        }
        if let Some(title) = self.clusters.waiting.remove(&index) {
            self.clusters.snapshots.insert(index, title);
            self.cache.clear();
        }
        drop(allocation);
    }

    /// Drops a node's cached bitmap so it draws live until a fresh raster
    /// arrives. See [`Rasters::invalidate`].
    pub(super) fn invalidate_snapshot(&mut self, index: usize) {
        self.rasters.invalidate(index);
    }

    /// Installs a finished background rasterization: bitmaps for nodes that
    /// already show one are staged (warmed, then cross-faded), and so is a
    /// first bitmap too big to upload in the frame that draws it; the rest go
    /// straight on screen, since there's nothing to preserve.
    pub(super) fn snapshots_ready(
        &mut self,
        epoch: u64,
        snapshots: Vec<(usize, text_raster::Snapshot)>,
    ) -> Task<Message> {
        // For a board since left: keyed by index, they'd go on whichever
        // cards here have those places.
        if epoch != self.board_epoch {
            return Task::none();
        }
        self.rasters.pending = false;
        let editing = self.editing.as_ref().map(|editing| editing.index);
        let now = Instant::now();
        for (index, snapshot) in snapshots {
            // Of a card changed while it was being made — more of an answer
            // streamed in, a resize. Stretched over it, its lines kept the
            // old card's wrap.
            if !self.nodes.get(index).is_some_and(|node| depicts(&snapshot, node)) {
                continue;
            }
            // A node being edited, or holding a search hit, is drawn live — the
            // first so the editor owns its text, the second because the
            // highlight is painted by the live path and a bitmap has none baked
            // in. `refresh_snapshots` won't *start* either, but a batch already
            // in flight when the search began would land here and quietly put a
            // bitmap back, leaving the hit unmarked until something else
            // invalidated it.
            if editing == Some(index) || self.is_search_match(index) {
                continue;
            }
            // A first bitmap had nothing to preserve, so it used to go straight
            // on screen — and one over the upload limit then drew as nothing
            // until iced's worker finished: a card that had just been edited
            // blinked empty between its live text and its bitmap. Staged, it
            // keeps the live text until the bitmap has warmed. Small ones
            // upload in-frame and gain nothing from waiting, which matters on a
            // board's first load, when every card is a first bitmap.
            if self.rasters.text.contains_key(&index) || snapshot.bytes >= ASYNC_UPLOAD_BYTES {
                self.rasters.staging.insert(
                    index,
                    StagedSnapshot { snapshot, since: now, opacity: 0.0 },
                );
            } else {
                self.rasters.text.insert(index, snapshot);
            }
        }
        self.cache.clear();
        // The camera may have moved again while rasterizing.
        self.refresh_snapshots()
    }

    /// Advances the staged bitmaps' warm-up and cross-fade, promoting any that
    /// have fully faded in — or, with no bitmap to fade over, any that have
    /// warmed: live text and its bitmap are the same glyphs, so there is
    /// nothing to ease between. Returns whether anything changed (so the caller
    /// can invalidate the canvas cache).
    pub(super) fn tick_snapshots(&mut self, now: Instant) -> bool {
        let mut promote = Vec::new();
        let mut fading = false;
        for (index, staged) in self.rasters.staging.iter_mut() {
            let elapsed = now.duration_since(staged.since);
            let opacity = if elapsed <= SNAPSHOT_WARM_UP {
                0.0
            } else {
                ((elapsed - SNAPSHOT_WARM_UP).as_secs_f32() / SNAPSHOT_FADE.as_secs_f32()).min(1.0)
            };
            if opacity != staged.opacity {
                fading = true;
            }
            staged.opacity = opacity;
            let replacing = self.rasters.text.contains_key(index);
            if opacity >= 1.0 || (!replacing && elapsed > SNAPSHOT_WARM_UP) {
                promote.push(*index);
            }
        }
        for index in &promote {
            if let Some(staged) = self.rasters.staging.remove(index) {
                self.rasters.text.insert(*index, staged.snapshot);
            }
        }
        fading || !promote.is_empty()
    }

    /// Kicks off background rasterization of text bitmaps that are visible
    /// but missing or whose captured zoom is too far from the current one.
    /// Old bitmaps keep drawing until replacements arrive, so nodes never
    /// go blank; never runs during a zoom gesture. One batch in flight at
    /// a time.
    pub(super) fn refresh_snapshots(&mut self) -> Task<Message> {
        // Live text mode draws straight from Markdown; no bitmaps to prepare.
        if !self.config.rasterize_text {
            return Task::none();
        }
        // A bitmap of a card since resized goes, rather than wait to be
        // replaced: resized by something that didn't say — an answer's cards
        // were widened to one width without — it'd be drawn until the new
        // one had faded in over it. The size only: this runs on every pan
        // event, and a change of text says so where it's made, while a batch
        // started before one is turned away as it lands (`snapshots_ready`).
        let on_screen = self.rasters.text.iter().map(|(index, snapshot)| (*index, snapshot));
        let warming = self.rasters.staging.iter().map(|(index, staged)| (*index, &staged.snapshot));
        let resized: Vec<usize> = on_screen
            .chain(warming)
            .filter(|(index, snapshot)| !self.nodes.get(*index).is_some_and(|node| snapshot.fits(node.width, node.height)))
            .map(|(index, _)| index)
            .collect();
        for index in resized {
            self.rasters.invalidate(index);
        }
        if self.rasters.pending {
            return Task::none();
        }

        let zoom = self.camera.zoom;
        let theme = self.theme;
        let center = self.config.center_card_text;
        let editing = self.editing.as_ref().map(|editing| editing.index);

        let jobs: Vec<(usize, String, f32, f32)> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                node.is_text()
                    && editing != Some(*index)
                    // Search matches draw live so their highlight shows.
                    && !self.is_search_match(*index)
                    && !self.rasters.staging.contains_key(index)
                    && self.node_visible(node)
                    && !self.rasters.text.get(index).is_some_and(|snapshot| {
                        (0.7..=2.0).contains(&(zoom / snapshot.zoom))
                            || (snapshot.scale
                                - text_raster::scale_for(zoom, node.width, node.height))
                            .abs()
                                < 0.001
                    })
            })
            .map(|(index, node)| (index, bitmap_text(node).to_owned(), node.width, node.height))
            .collect();

        if jobs.is_empty() {
            return Task::none();
        }

        self.rasters.pending = true;
        let epoch = self.board_epoch;
        Task::perform(
            async move {
                jobs.into_iter()
                    .filter_map(|(index, text, width, height)| {
                        let snapshot = text_raster::snapshot_markdown(
                            &text,
                            width,
                            height,
                            text_raster::card_text_wrap(width),
                            super::FONT_SIZE,
                            theme,
                            center,
                            zoom,
                        )?;
                        Some((index, snapshot))
                    })
                    .collect()
            },
            move |snapshots| Message::SnapshotsReady(epoch, snapshots),
        )
    }

    /// Recomputes clusters off the main thread. Coalesces bursts: if one is
    /// already running, just marks the result stale so it reruns once on
    /// completion. Called only after settled geometry/edge changes, never
    /// during a drag or on pan/zoom.
    pub(super) fn schedule_recluster(&mut self) -> Task<Message> {
        if self.clusters.pending {
            self.clusters.dirty = true;
            return Task::none();
        }
        self.clusters.pending = true;
        self.clusters.dirty = false;
        let nodes = self.nodes.clone();
        let edges = self.edges.clone();
        let cluster_zoom = self.config.cluster_zoom;
        let theme = self.theme;
        let center = self.config.center_card_text;
        let rasterize = self.config.rasterize_text;
        let epoch = self.board_epoch;
        Task::perform(
            async move {
                let clustering = cluster::compute(&nodes, &edges, &theme);
                let snapshots =
                    cluster_snapshots(&clustering.clusters, cluster_zoom, theme, center, rasterize);
                (clustering, snapshots)
            },
            move |(clustering, snapshots)| Message::ClustersReady(epoch, clustering, snapshots),
        )
    }

    /// Installs a finished background clustering, rerunning it once more if
    /// changes arrived while it was in flight.
    pub(super) fn clusters_ready(
        &mut self,
        epoch: u64,
        clustering: cluster::Clustering,
        snapshots: HashMap<usize, text_raster::Snapshot>,
    ) -> Task<Message> {
        // For a board since left — whose titles over this one's cards were
        // the flicker a switch used to show.
        if epoch != self.board_epoch {
            return Task::none();
        }
        self.clusters.list = clustering.clusters;
        self.clusters.links = clustering.links;
        self.clusters.pending = false;
        let titles = self.install_titles(snapshots);
        if self.clusters.dirty {
            return Task::batch([titles, self.schedule_recluster()]);
        }
        titles
    }
}

#[cfg(test)]
mod tests {
    use super::super::update_tests::app_with;
    use super::*;

    #[test]
    fn a_title_waits_for_its_upload_and_one_for_old_clusters_is_dropped() {
        let mut app = app_with(Vec::new(), Vec::new());
        let theme = app.theme;
        let title = || text_raster::snapshot_markdown("Title", 600.0, 400.0, 560.0, 40.0, theme, true, 0.4).expect("a bitmap");
        let _ = app.install_titles(HashMap::from([(0, title())]));
        assert!(app.clusters.snapshots.is_empty(), "drawn live until it's on the GPU");
        let (epoch, earlier) = (app.board_epoch, app.clusters.generation);

        // Clustered again before that upload landed: it's for clusters gone.
        let _ = app.install_titles(HashMap::from([(0, title())]));
        app.title_uploaded(epoch, earlier, 0, None);
        assert!(app.clusters.snapshots.is_empty());
        app.title_uploaded(epoch, app.clusters.generation, 0, None);
        assert!(app.clusters.snapshots.contains_key(&0));
    }
}
