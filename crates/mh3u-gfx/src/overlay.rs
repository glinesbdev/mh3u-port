//! The mod overlay: `egui` drawn over the game's picture, opened and closed with F1 or a gamepad's Back + right shoulder button. While
//! open it takes all input (the game gets none; see `mh3u_rt::input`). Closed, it costs one atomic load a frame, plus a look at the
//! live save four times a second while the wishlist has something on it (for the notices).
//!
//! The window's events reach egui on the window's thread (`egui-winit`), which hands the input it makes to the game thread through
//! `INPUT`; the game thread runs egui and draws it at the end of the present pass (`egui-wgpu`).
//!
//! What it shows is the blacksmith wishlist (`mh3u_mods`): pieces picked from the recipes, what each needs against the item box and
//! pouch of the save the game is playing, a shopping list of it all, and a notice when a piece becomes craftable.

use mh3u_mods::data::{GameData, KINDS};
use mh3u_mods::live;
use mh3u_mods::wishlist::{Craftable, Piece, Wishlist};
use mh3u_rt::input::{OVERLAY_ENABLED, OVERLAY_OPEN, OVERLAY_PRESSES, OVERLAY_TOGGLE, button};
use mh3u_rt::mods::{self, Searching};
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// The egui context the window's thread and the game thread share.
pub static CONTEXT: LazyLock<egui::Context> = LazyLock::new(egui::Context::default);
/// The keyboard and mouse bindings, for the overlay's Controls section (empty with them off).
pub static CONTROLS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// The input the window's thread gathered since the game thread last took it.
pub static INPUT: Mutex<Option<egui::RawInput>> = Mutex::new(None);

/// Whether the overlay is on (`MH3U_OVERLAY=off` turns it off); says so to the input.
pub fn enabled() -> bool {
    let on = !matches!(std::env::var("MH3U_OVERLAY").ok().as_deref().map(str::trim), Some("off" | "0"));
    OVERLAY_ENABLED.store(on, Relaxed);
    on
}

/// Adds input from the window's thread to what the game thread will take: the events in order, the newest of everything else.
pub fn push_input(raw: egui::RawInput) {
    let mut slot = INPUT.lock().unwrap_or_else(|e| e.into_inner());
    match slot.as_mut() {
        Some(old) => {
            let mut events = std::mem::take(&mut old.events);
            events.extend(raw.events.iter().cloned());
            let mut dropped = std::mem::take(&mut old.dropped_files);
            dropped.extend(raw.dropped_files.iter().cloned());
            *old = raw;
            old.events = events;
            old.dropped_files = dropped;
        }
        None => *slot = Some(raw),
    }
}

/// The input for one egui frame: what the window's thread gathered (`pending`), or, on a frame with no window events, the last
/// input without its events. The window's size, scale and focus have to be there every frame: an input made from nothing has a scale
/// of 1, and frames alternating between that and the real scale moved everything at every frame (the overlay flickered and a text
/// box could not keep the focus).
pub fn next_input(pending: Option<egui::RawInput>, last: &mut egui::RawInput) -> egui::RawInput {
    match pending {
        Some(raw) => {
            *last = raw.clone();
            last.events.clear();
            last.dropped_files.clear();
            last.hovered_files.clear();
            raw
        }
        None => last.clone(),
    }
}

/// How long a notice stays.
const NOTICE: Duration = Duration::from_secs(6);
/// How often the save is looked at while closed, in frames, and how long after a failed search the next starts.
const LOOK_EVERY: u64 = 15;

/// Following the live save: wait (with a reason, after a failed search), search, check the candidates from a snapshot, follow.
enum Save {
    Waiting {
        until: Instant,
        reason: String,
    },
    Searching {
        names: Vec<String>,
    },
    /// The candidates are being copied at a frame (asked for at frame `asked`).
    Checking {
        names: Vec<String>,
        candidates: Vec<u32>,
        asked: u64,
    },
    /// Found at this address (with the name it carries, to follow it when its header is not the expected one), and its latest contents.
    At(u32, String, Option<Box<mh3u_mods::companion::save::Save>>),
}

/// The waits between searches that find nothing: longer each time, up to a minute.
const BACKOFF: [u64; 5] = [5, 10, 20, 40, 60];

pub struct Overlay {
    renderer: Option<egui_wgpu::Renderer>,
    open: bool,
    data: Arc<Mutex<Option<Result<GameData, String>>>>,
    wishlist: Wishlist,
    path: PathBuf,
    save: Save,
    save_dir: PathBuf,
    failures: usize,
    /// The last reason logged (each is logged once, not at every try).
    logged: String,
    craftable: Craftable,
    notices: Vec<(String, Instant)>,
    frame: u64,
    /// The picker: the kind shown and the filter typed.
    kind: u8,
    filter: String,
    /// The last input (see `next_input`).
    last_input: egui::RawInput,
    /// When the live save is read (on hook events, see `events`), and the last events, for the feed.
    refresh: crate::events::Refresh,
    feed: std::collections::VecDeque<String>,
    /// Gains the save does not have yet (a quest's pickups).
    provisional: crate::events::Provisional,
    /// F10 presses seen (see `follow_save`).
    f10: u32,
}

/// How many events the feed shows.
const FEED: usize = 8;

impl Overlay {
    /// The overlay with the game's data read from the dump's base folder (on a thread of its own: it takes a moment) and the wishlist
    /// kept in `dir`.
    pub fn new(dump_base: PathBuf, dir: PathBuf, save_dir: PathBuf) -> Overlay {
        let data = Arc::new(Mutex::new(None));
        let slot = data.clone();
        std::thread::spawn(move || {
            let loaded = GameData::load(&dump_base).map_err(|e| format!("{e:#}"));
            match &loaded {
                Ok(d) => eprintln!(
                    "[overlay] game data: {} items, {} recipes, {} upgrades",
                    d.items.len(),
                    d.recipes.len(),
                    d.upgrades.len()
                ),
                Err(e) => eprintln!("[overlay] no game data: {e}"),
            }
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(loaded);
        });
        let path = dir.join("wishlist.json");
        let wishlist = Wishlist::load(&path);
        eprintln!(
            "[overlay] F1 or Back + RB opens it; wishlist {} ({} pieces)",
            path.display(),
            wishlist.pieces.len()
        );
        Overlay {
            renderer: None,
            open: false,
            data,
            wishlist,
            path,
            save: Save::Waiting {
                until: Instant::now(),
                reason: String::new(),
            },
            save_dir,
            failures: 0,
            logged: String::new(),
            craftable: Craftable::default(),
            notices: Vec::new(),
            frame: 0,
            kind: 5,
            filter: String::new(),
            last_input: egui::RawInput::default(),
            refresh: crate::events::Refresh::new(),
            feed: Default::default(),
            provisional: Default::default(),
            f10: 0,
        }
    }

    /// Each frame, from the present pass: opens or closes on a toggle, keeps the live save up to date, and draws when open or when a
    /// notice is showing.
    pub fn frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        size: (u32, u32),
    ) {
        if OVERLAY_TOGGLE.swap(false, Relaxed) {
            self.open = !self.open;
            OVERLAY_OPEN.store(self.open, Relaxed);
            OVERLAY_PRESSES.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
        self.frame += 1;
        // the save is followed while the overlay is open, now and then for a wishlist, and always for the live watch (PR #17 run:
        // with an empty wishlist and the overlay never opened, F10 found no save to search from)
        if self.open || mods::watching_live() || (!self.wishlist.pieces.is_empty() && self.frame.is_multiple_of(LOOK_EVERY)) {
            self.follow_save();
        }
        self.notices.retain(|(_, t)| t.elapsed() < NOTICE);
        if !self.open && self.notices.is_empty() {
            // nothing to draw: the window's size and scale are kept, its events are not wanted
            let pending = INPUT.lock().unwrap_or_else(|e| e.into_inner()).take();
            next_input(pending, &mut self.last_input);
            return;
        }
        self.draw(device, queue, enc, view, format, size);
    }

    /// Finds the live save (the search runs on its own thread; its candidates are checked from a snapshot taken at a frame) and reads
    /// it from the snapshot of each frame.
    fn follow_save(&mut self) {
        let f10 = mods::KEY_F10.load(std::sync::atomic::Ordering::Relaxed);
        if f10 != self.f10 && !matches!(self.save, Save::At(..)) {
            self.f10 = f10;
            eprintln!(
                "[watch-live] F10: the live save is not found yet, so there is no pouch to search for; wait for `[overlay] the live save is at`"
            );
        }
        match &mut self.save {
            Save::Waiting { until, .. } => {
                if Instant::now() >= *until {
                    let names = live::hunter_names(&self.save_dir);
                    let patterns = live::name_patterns(&names);
                    mods::search(
                        live::RANGES.to_vec(),
                        Arc::new(move |piece: &[u8], base: u32, out: &mut Vec<u32>| live::find_candidates(piece, base, &patterns, out)),
                    );
                    self.save = Save::Searching { names };
                }
            }
            Save::Searching { names } => {
                if let Searching::Done(candidates) = mods::searching() {
                    let names = std::mem::take(names);
                    // as many as fit in what can be watched
                    let candidates: Vec<u32> = candidates.into_iter().take((mods::MOST_WATCHED / live::LEN) as usize).collect();
                    mods::watch(&candidates.iter().map(|&c| (c, live::LEN)).collect::<Vec<_>>());
                    let asked = mh3u_rt::FRAMES.load(Relaxed);
                    self.save = Save::Checking { names, candidates, asked };
                }
            }
            Save::Checking { names, candidates, asked } => {
                let snap = mods::latest();
                if snap.frame <= *asked && !candidates.is_empty() {
                    return;
                }
                let blocks: Vec<(u32, &[u8])> = candidates
                    .iter()
                    .filter_map(|&c| Some((c, snap.bytes(c, live::LEN as usize)?)))
                    .collect();
                match live::choose(&blocks, names) {
                    Ok((at, why)) => {
                        let name = blocks
                            .iter()
                            .find(|b| b.0 == at)
                            .and_then(|b| live::parse(b.1, at, None).or_else(|| mh3u_mods::companion::save::Save::parse(b.1).ok()))
                            .map(|s| s.hunter_name)
                            .unwrap_or_default();
                        eprintln!("[overlay] the live save is at {at:#x} ({why}): {name}");
                        mods::watch(&[(at, live::LEN)]);
                        // MH3U_WATCH=live: log the stores to it, to find the functions to hook (docs/hooks.md)
                        let (lo, hi) = live::WATCH_SPAN;
                        // MH3U_WATCH_AT=<address>: watch a block with the save's layout there instead (the quest's working save,
                        // copied from the save when a quest starts; docs/hooks.md)
                        let block = std::env::var("MH3U_WATCH_AT")
                            .ok()
                            .and_then(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok())
                            .unwrap_or(at);
                        if mods::watch_writes(block + lo, block + hi, Box::new(move |a| live::field(a - block)))
                            && std::env::var_os("MH3U_WATCH_COPY").is_some()
                        {
                            // and, asked for with MH3U_WATCH_COPY, other copies of the pouch (run 30 found the save itself is the one written)
                            const POUCH: i64 = 0xd0;
                            mods::watch_copies(
                                at + POUCH as u32,
                                24 * 4,
                                live::RANGES.to_vec(),
                                (i64::from(lo) - POUCH, i64::from(hi) - POUCH),
                                std::sync::Arc::new(|d: i64| match u32::try_from(d + POUCH) {
                                    Ok(o) => live::field(o),
                                    Err(_) => format!("pouch {d}"),
                                }),
                            );
                        }
                        self.failures = 0;
                        self.logged.clear();
                        self.save = Save::At(at, name, None);
                    }
                    Err(reason) => {
                        mods::watch(&[]);
                        self.wait(reason);
                    }
                }
            }
            Save::At(at, name, save) => {
                let at = *at;
                // F10 (MH3U_WATCH=live): look for copies of the pouch as it is now (on a quest: the quest pouch) and watch one
                let f10 = mods::KEY_F10.load(std::sync::atomic::Ordering::Relaxed);
                if f10 != self.f10 {
                    self.f10 = f10;
                    const POUCH: i64 = 0xd0;
                    let (lo, hi) = live::WATCH_SPAN;
                    eprintln!("[watch-live] F10: searching for copies of the pouch");
                    mods::watch_copies(
                        at + POUCH as u32,
                        24 * 4,
                        live::RANGES.to_vec(),
                        (i64::from(lo) - POUCH, i64::from(hi) - POUCH),
                        Arc::new(|d: i64| match u32::try_from(d + POUCH) {
                            Ok(o) => live::field(o),
                            Err(_) => format!("pouch {d}"),
                        }),
                    );
                }
                let events = crate::events::drain();
                if !events.is_empty() {
                    let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
                    let name_of = |id: u16| match data.as_ref() {
                        Some(Ok(d)) => d.item_name(id).to_string(),
                        _ => format!("item {id}"),
                    };
                    for e in &events {
                        match *e {
                            crate::events::Event::ItemGain { item, .. } => {
                                let now = save.as_ref().map_or(0, |s| s.item_count(item));
                                self.provisional.gain(item, now);
                            }
                            crate::events::Event::QuestEnd => self.provisional.clear(),
                            _ => {}
                        }
                        if let Some(line) = crate::events::describe(e, &name_of) {
                            if self.feed.len() == FEED {
                                self.feed.pop_front();
                            }
                            self.feed.push_back(line);
                        }
                    }
                }
                if !self.refresh.due(Instant::now(), crate::events::hooked(), !events.is_empty()) {
                    return;
                }
                let snap = mods::latest();
                let Some(bytes) = snap.bytes(at, live::LEN as usize) else { return };
                match live::parse(bytes, at, Some(name)) {
                    Some(parsed) => {
                        self.provisional.observe(|i| parsed.item_count(i));
                        *save = Some(Box::new(parsed));
                        self.check_craftable();
                    }
                    None => {
                        // the hunter was unloaded (or the block moved): look again
                        mods::watch(&[]);
                        self.craftable.forget();
                        self.wait("the save that was found is not there any more (hunter unloaded?)".into());
                    }
                }
            }
        }
    }

    /// No live save for now: says why (once per reason) and waits longer each time before searching again.
    fn wait(&mut self, reason: String) {
        let secs = BACKOFF[self.failures.min(BACKOFF.len() - 1)];
        self.failures += 1;
        if reason != self.logged {
            eprintln!("[overlay] no live save: {reason}; searching again in {secs} s, then less often");
            self.logged = reason.clone();
        }
        self.save = Save::Waiting {
            until: Instant::now() + Duration::from_secs(secs),
            reason,
        };
    }

    /// Notices for the pieces that just became craftable.
    fn check_craftable(&mut self) {
        let Save::At(_, _, Some(save)) = &self.save else { return };
        let shared = self.data.clone();
        let guard = shared.lock().unwrap_or_else(|e| e.into_inner());
        let Some(Ok(data)) = guard.as_ref() else { return };
        let statuses = self.wishlist.status(
            |p| data.making(p.kind, p.id).map(|m| m.materials),
            |i| save.item_count(i) + self.provisional.extra(i),
        );
        mh3u_rt::native_text::set(native_line(&statuses));
        mh3u_rt::native_menu::set_entries(Some(page_entries(
            &statuses,
            |p| data.piece_name(p.kind, p.id),
            |i| data.item_name(i),
        )));
        let new = self.craftable.update(&statuses);
        let names: Vec<String> = new
            .iter()
            .map(|p| data.piece_name(p.kind, p.id).unwrap_or("?").to_string())
            .collect();
        drop(guard);
        for n in names {
            eprintln!("[overlay] craftable now: {n}");
            self.notices.push((format!("⚒ {n} can be made now"), Instant::now()));
        }
    }

    fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        size: (u32, u32),
    ) {
        let ctx = CONTEXT.clone();
        let pending = INPUT.lock().unwrap_or_else(|e| e.into_inner()).take();
        let mut raw = next_input(pending, &mut self.last_input);
        if !self.open {
            // the notices only: no input for them
            raw.events.clear();
        }
        let ppp = raw
            .viewports
            .get(&raw.viewport_id)
            .and_then(|v| v.native_pixels_per_point)
            .unwrap_or(1.0);
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size.0 as f32 / ppp, size.1 as f32 / ppp),
        ));
        // the gamepad moves through the overlay: up and down between the controls, A presses, B closes
        for b in OVERLAY_PRESSES.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
            let key = |key, shift| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    shift,
                    ..Default::default()
                },
            };
            match b {
                button::DOWN => raw.events.push(key(egui::Key::Tab, false)),
                button::UP => raw.events.push(key(egui::Key::Tab, true)),
                button::A => raw.events.push(key(egui::Key::Enter, false)),
                button::B => OVERLAY_TOGGLE.store(true, Relaxed),
                _ => {}
            }
        }
        let output = ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            self.ui(&ctx);
        });
        let renderer = self
            .renderer
            .get_or_insert_with(|| egui_wgpu::Renderer::new(device, format, Default::default()));
        let jobs = ctx.tessellate(output.shapes, output.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [size.0, size.1],
            pixels_per_point: output.pixels_per_point,
        };
        let mut textures = output.textures_delta;
        for (id, deltas) in textures.set.drain() {
            for delta in &deltas {
                renderer.update_texture(device, queue, id, delta);
            }
        }
        let extra = renderer.update_buffers(device, queue, enc, &jobs, &screen);
        {
            let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.render(&mut pass.forget_lifetime(), &jobs, &screen);
        }
        if !extra.is_empty() {
            queue.submit(extra);
        }
        for id in textures.free.drain() {
            renderer.free_texture(&id);
        }
    }

    fn ui(&mut self, ctx: &egui::Context) {
        for (k, (text, _)) in self.notices.iter().enumerate() {
            egui::Area::new(egui::Id::new(("notice", k)))
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 16.0 + 40.0 * k as f32))
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(egui::RichText::new(text).strong()));
                });
        }
        if !self.open {
            return;
        }
        let mut changed = false;
        let shared = self.data.clone();
        let guard = shared.lock().unwrap_or_else(|e| e.into_inner());
        egui::Window::new("Blacksmith wishlist")
            .default_pos([24.0, 24.0])
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label("F1 or Back + RB closes this. The game is paused for input while it is open.");
                let data = match guard.as_ref() {
                    None => {
                        ui.label("Reading the game's data from the dump…");
                        return;
                    }
                    Some(Err(e)) => {
                        ui.colored_label(egui::Color32::LIGHT_RED, format!("No game data: {e}"));
                        return;
                    }
                    Some(Ok(d)) => d,
                };
                let save = match &self.save {
                    Save::At(at, _, Some(s)) => {
                        ui.label(format!("Hunter: {} (live save at {at:#x})", s.hunter_name));
                        Some(s)
                    }
                    Save::At(..) | Save::Checking { .. } => {
                        ui.label("Checking what the search found…");
                        None
                    }
                    Save::Searching { .. } => {
                        if let Searching::Running { done, of } = mods::searching() {
                            ui.label(format!("Looking for the live save in memory… {}%", done * 100 / of.max(1)));
                        }
                        None
                    }
                    Save::Waiting { until, reason } => {
                        if !reason.is_empty() {
                            ui.colored_label(egui::Color32::LIGHT_RED, format!("No live save: {reason}"));
                            let left = until.saturating_duration_since(Instant::now()).as_secs();
                            ui.label(format!(
                                "Looking again in {left} s (sooner after loading a hunter? reopening does not hurry it)"
                            ));
                        }
                        None
                    }
                };
                let provisional = &self.provisional;
                let have = |item: u16| save.map_or(0, |s| s.item_count(item)) + provisional.extra(item);
                ui.separator();
                // the picker
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("kind")
                        .selected_text(KINDS.iter().find(|k| k.0 == self.kind).map_or("?", |k| k.1))
                        .show_ui(ui, |ui| {
                            for &(k, name, _) in KINDS {
                                ui.selectable_value(&mut self.kind, k, name);
                            }
                        });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("filter by name")
                            .desired_width(200.0),
                    );
                });
                let needle = self.filter.to_lowercase();
                egui::ScrollArea::vertical().id_salt("pieces").max_height(160.0).show(ui, |ui| {
                    for (id, name) in data.pieces(self.kind) {
                        if !needle.is_empty() && !name.to_lowercase().contains(&needle) {
                            continue;
                        }
                        let piece = Piece { kind: self.kind, id };
                        ui.horizontal(|ui| {
                            let on = self.wishlist.pieces.contains(&piece);
                            if ui.add_enabled(!on, egui::Button::new("+")).clicked() {
                                changed |= self.wishlist.add(piece);
                            }
                            ui.label(name);
                        });
                    }
                });
                ui.separator();
                let quest: Vec<String> = self
                    .provisional
                    .items()
                    .map(|(i, n)| format!("+{n} {}", data.item_name(i)))
                    .collect();
                if !quest.is_empty() {
                    ui.label(format!("Counted, not in the save yet (+N this quest): {}", quest.join(", ")));
                }
                let statuses = self.wishlist.status(|p| data.making(p.kind, p.id).map(|m| m.materials), have);
                if statuses.is_empty() {
                    ui.label("Nothing wished for yet: pick pieces above.");
                }
                let mut remove = None;
                egui::ScrollArea::vertical().id_salt("wished").max_height(260.0).show(ui, |ui| {
                    for s in &statuses {
                        let p = s.piece;
                        let name = data.piece_name(p.kind, p.id).unwrap_or("?");
                        let missing: u32 = s.materials.iter().map(|n| n.missing()).sum();
                        let head = if s.craftable {
                            format!("✔ {name}: ready")
                        } else {
                            format!("{name}: {missing} missing")
                        };
                        ui.horizontal(|ui| {
                            if ui.small_button("✖").on_hover_text("take it off the list").clicked() {
                                remove = Some(p);
                            }
                            egui::CollapsingHeader::new(head).id_salt(("wish", p.kind, p.id)).show(ui, |ui| {
                                if let Some(m) = data.making(p.kind, p.id)
                                    && !m.from.is_empty()
                                {
                                    let from: Vec<&str> = m.from.iter().filter_map(|&f| data.piece_name(p.kind, f)).collect();
                                    ui.label(format!("upgraded from {}", from.join(" or ")));
                                }
                                egui::Grid::new(("materials", p.kind, p.id)).striped(true).show(ui, |ui| {
                                    for n in &s.materials {
                                        ui.label(data.item_name(n.item));
                                        let text = format!("{} / {}", n.have, n.need);
                                        if n.missing() == 0 {
                                            ui.colored_label(egui::Color32::LIGHT_GREEN, text);
                                        } else {
                                            ui.label(text);
                                        }
                                        ui.end_row();
                                    }
                                });
                            });
                        });
                    }
                });
                if let Some(p) = remove {
                    self.wishlist.remove(p);
                    changed = true;
                }
                if !statuses.is_empty() {
                    ui.separator();
                    ui.label(egui::RichText::new("Shopping list").strong());
                    egui::Grid::new("shopping").striped(true).show(ui, |ui| {
                        ui.label("item");
                        ui.label("have");
                        ui.label("need");
                        ui.label("missing");
                        ui.end_row();
                        for n in Wishlist::shopping(&statuses) {
                            ui.label(data.item_name(n.item));
                            ui.label(n.have.to_string());
                            ui.label(n.need.to_string());
                            if n.missing() > 0 {
                                ui.colored_label(egui::Color32::LIGHT_RED, n.missing().to_string());
                            } else {
                                ui.label("0");
                            }
                            ui.end_row();
                        }
                    });
                }
            });
        drop(guard);
        if !self.feed.is_empty() {
            egui::Window::new("Events").default_pos([560.0, 300.0]).show(ctx, |ui| {
                for line in self.feed.iter().rev() {
                    ui.label(line);
                }
            });
        }
        let controls = CONTROLS.lock().unwrap_or_else(|e| e.into_inner()).clone();
        egui::Window::new("Controls")
            .default_pos([560.0, 24.0])
            .default_open(false)
            .show(ctx, |ui| {
                if controls.is_empty() {
                    ui.label("Keyboard and mouse controls are off (MH3U_KBM=off): the mouse is the GamePad's touch screen.");
                    return;
                }
                ui.label("Keyboard and mouse (edit input/bindings.json in the build folder, then restart):");
                egui::Grid::new("controls").striped(true).show(ui, |ui| {
                    for (what, keys) in &controls {
                        ui.label(what);
                        ui.label(keys);
                        ui.end_row();
                    }
                });
                ui.label("F1: this overlay. F11 or Alt+Enter: fullscreen.");
            });
        if changed && let Err(e) = self.wishlist.save(&self.path) {
            eprintln!("[overlay] could not save the wishlist to {}: {e}", self.path.display());
        }
    }
}

/// The wishlist's status in a line for the game's item menu (`MH3U_NATIVE_TEXT`): `None` when the wishlist is empty.
fn native_line(statuses: &[mh3u_mods::wishlist::Status]) -> Option<String> {
    if statuses.is_empty() {
        return None;
    }
    let ready = statuses.iter().filter(|s| s.craftable).count();
    if ready > 0 {
        return Some(format!("Wishlist: {ready} of {} ready", statuses.len()));
    }
    let missing: u32 = statuses.iter().flat_map(|s| s.materials.iter()).map(|n| n.missing()).sum();
    Some(format!("Wishlist: need {missing} items"))
}

/// The wishlist for the native Mods page (`MH3U_NATIVE_MENU`): each piece's name, whether it is ready and its materials.
fn page_entries<'a>(
    statuses: &[mh3u_mods::wishlist::Status],
    name: impl Fn(mh3u_mods::wishlist::Piece) -> Option<&'a str>,
    item_name: impl Fn(u16) -> &'a str,
) -> Vec<mh3u_rt::native_menu::Entry> {
    statuses
        .iter()
        .map(|s| mh3u_rt::native_menu::Entry {
            name: name(s.piece).unwrap_or("?").to_string(),
            ready: s.craftable,
            materials: s
                .materials
                .iter()
                .map(|m| mh3u_rt::native_menu::Material {
                    item: m.item,
                    name: item_name(m.item).to_string(),
                    have: m.have,
                    need: m.need,
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_from_the_window_keeps_every_event_and_the_newest_of_the_rest() {
        let ev = |c: &str| egui::Event::Text(c.into());
        push_input(egui::RawInput {
            events: vec![ev("a")],
            time: Some(1.0),
            ..Default::default()
        });
        push_input(egui::RawInput {
            events: vec![ev("b")],
            time: Some(2.0),
            ..Default::default()
        });
        let raw = INPUT.lock().unwrap().take().unwrap();
        assert_eq!(raw.events, [ev("a"), ev("b")]);
        assert_eq!(raw.time, Some(2.0));
    }

    #[test]
    fn a_frame_without_window_events_keeps_the_scale_and_focus_of_the_last() {
        let mut last = egui::RawInput::default();
        let mut raw = egui::RawInput {
            focused: true,
            events: vec![egui::Event::Text("a".into())],
            ..Default::default()
        };
        raw.viewports.entry(raw.viewport_id).or_default().native_pixels_per_point = Some(1.5);
        let got = next_input(Some(raw), &mut last);
        assert_eq!(got.events.len(), 1);
        // the next frame, no events: the same scale, no events again
        let quiet = next_input(None, &mut last);
        assert!(quiet.events.is_empty());
        assert_eq!(quiet.viewports[&quiet.viewport_id].native_pixels_per_point, Some(1.5));
        assert!(quiet.focused);
    }

    #[test]
    fn it_opens_draws_and_closes_on_a_toggle_and_draws_nothing_closed() {
        let d = crate::Display::noop();
        let dir = std::env::temp_dir().join(format!("mh3u-overlay-{}", std::process::id()));
        // no dump here: the overlay says so instead of the wishlist
        let mut o = Overlay::new(dir.join("no-dump"), dir.clone(), dir.join("save"));
        let target = d.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 640,
                height: 360,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let frame = |o: &mut Overlay| {
            let mut enc = d.device.create_command_encoder(&Default::default());
            o.frame(&d.device, &d.queue, &mut enc, &view, wgpu::TextureFormat::Rgba8Unorm, (640, 360));
            d.queue.submit([enc.finish()]);
        };
        frame(&mut o);
        assert!(o.renderer.is_none(), "closed with no notices: egui does not run");
        OVERLAY_TOGGLE.store(true, Relaxed);
        frame(&mut o);
        frame(&mut o);
        assert!(OVERLAY_OPEN.load(Relaxed));
        assert!(o.renderer.is_some());
        OVERLAY_TOGGLE.store(true, Relaxed);
        frame(&mut o);
        assert!(!OVERLAY_OPEN.load(Relaxed));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_native_line_says_how_many_are_ready_or_what_is_missing() {
        use mh3u_mods::wishlist::{Need, Status};
        let piece = |id| Piece { kind: 0, id };
        let need = |need, have| Need { item: 1, need, have };
        assert_eq!(native_line(&[]), None);
        let short = Status {
            piece: piece(1),
            materials: vec![need(3, 1), need(2, 0)],
            craftable: false,
        };
        assert_eq!(native_line(std::slice::from_ref(&short)).unwrap(), "Wishlist: need 4 items");
        let done = Status {
            piece: piece(2),
            materials: vec![need(1, 1)],
            craftable: true,
        };
        assert_eq!(native_line(&[short, done]).unwrap(), "Wishlist: 1 of 2 ready");
    }
}
