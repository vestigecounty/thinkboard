//! Text bitmaps and semantic-zoom clusters — the two caches the board draws
//! from, both recomputed off the main thread.
//!
//! Every text node is drawn as a bitmap rasterized at a reference zoom and
//! GPU-scaled from there, so gestures never touch the glyph rasterizer. A
//! bitmap is only re-rasterized when the settled zoom drifts outside its
//! quality band or its text changes; a fresh one is warmed invisibly and
//! cross-faded in, because iced uploads large images on a worker thread and
//! draws nothing until that finishes.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::Task;

use super::{App, Message};
use crate::canvas::text_raster;
use crate::document::cluster;
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
    pending: bool,
    /// A change arrived while a recluster was in flight; recompute again.
    dirty: bool,
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

impl App {
    /// The initial clustering, computed synchronously at startup (there is no
    /// board on screen yet to keep responsive).
    pub(super) fn initial_clusters(&self) -> Clusters {
        let clustering = cluster::compute(&self.nodes, &self.edges, &self.theme);
        let snapshots = cluster_snapshots(
            &clustering.clusters,
            self.config.cluster_zoom,
            self.theme,
            self.config.center_node_text,
            self.config.rasterize_text,
        );
        Clusters {
            list: clustering.clusters,
            links: clustering.links,
            snapshots,
            pending: false,
            dirty: false,
        }
    }

    /// Drops a node's cached bitmap so it draws live until a fresh raster
    /// arrives. See [`Rasters::invalidate`].
    pub(super) fn invalidate_snapshot(&mut self, index: usize) {
        self.rasters.invalidate(index);
    }

    /// Installs a finished background rasterization: bitmaps for nodes that
    /// already show one are staged (warmed, then cross-faded); the rest go
    /// straight on screen, since there's nothing to preserve.
    pub(super) fn snapshots_ready(
        &mut self,
        snapshots: Vec<(usize, text_raster::Snapshot)>,
    ) -> Task<Message> {
        self.rasters.pending = false;
        let editing = self.editing.as_ref().map(|editing| editing.index);
        let now = Instant::now();
        for (index, snapshot) in snapshots {
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
            if self.rasters.text.contains_key(&index) {
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
    /// have fully faded in. Returns whether anything changed (so the caller can
    /// invalidate the canvas cache).
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
            if opacity >= 1.0 {
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
        if self.rasters.pending {
            return Task::none();
        }

        let zoom = self.camera.zoom;
        let theme = self.theme;
        let center = self.config.center_node_text;
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
            .map(|(index, node)| {
                let text = node.text.as_deref().unwrap_or("(no content)").to_owned();
                (index, text, node.width, node.height)
            })
            .collect();

        if jobs.is_empty() {
            return Task::none();
        }

        self.rasters.pending = true;
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
            Message::SnapshotsReady,
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
        let center = self.config.center_node_text;
        let rasterize = self.config.rasterize_text;
        Task::perform(
            async move {
                let clustering = cluster::compute(&nodes, &edges, &theme);
                let snapshots =
                    cluster_snapshots(&clustering.clusters, cluster_zoom, theme, center, rasterize);
                (clustering, snapshots)
            },
            |(clustering, snapshots)| Message::ClustersReady(clustering, snapshots),
        )
    }

    /// Installs a finished background clustering, rerunning it once more if
    /// changes arrived while it was in flight.
    pub(super) fn clusters_ready(
        &mut self,
        clustering: cluster::Clustering,
        snapshots: HashMap<usize, text_raster::Snapshot>,
    ) -> Task<Message> {
        self.clusters.list = clustering.clusters;
        self.clusters.links = clustering.links;
        self.clusters.snapshots = snapshots;
        self.clusters.pending = false;
        self.cache.clear();
        if self.clusters.dirty {
            return self.schedule_recluster();
        }
        Task::none()
    }
}
