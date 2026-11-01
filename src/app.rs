use crate::scan::{self, NodeId, Progress, Scan, Tree};
use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2, ecolor::HsvaGamma,
};
use std::collections::HashSet;
use std::f32::consts::TAU;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const ROW_H: f32 = 28.0;
/// Main text size, in points.
const TEXT: f32 = 14.5;
/// Secondary text (apparent size, percentages, details).
const SMALL: f32 = 12.5;
const FULL_DISK_ACCESS_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
/// How long a confirmation stays in the status bar.
const NOTICE_FOR: Duration = Duration::from_secs(8);

/// Use the macOS system fonts (as in System Settings and Finder). egui's own
/// fonts stay at the end of each list, so the app still works without them.
fn install_system_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let families = [
        (egui::FontFamily::Proportional, ["SFNS.ttf", "SFHebrew.ttf"].as_slice()),
        (egui::FontFamily::Monospace, ["SFNSMono.ttf", "SFNS.ttf"].as_slice()),
    ];
    for (family, files) in families {
        let mut names = Vec::new();
        for file in files {
            let Ok(bytes) = std::fs::read(format!("/System/Library/Fonts/{file}")) else { continue };
            fonts.font_data.insert(file.to_string(), egui::FontData::from_owned(bytes));
            names.push(file.to_string());
        }
        fonts.families.entry(family).or_default().splice(0..0, names);
    }
    ctx.set_fonts(fonts);
}
/// Rings drawn around the focused folder in the sunburst.
const RINGS: usize = 6;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Chart,
    Largest,
    Types,
    Ages,
}

enum Action {
    /// Select from the tree itself (no scrolling).
    Select(NodeId),
    /// Select, expand its ancestors, scroll the tree to it and focus the chart.
    Locate(NodeId),
    Toggle(NodeId),
    Reveal(NodeId),
    Open(NodeId),
    CopyPath(NodeId),
    Trash(NodeId),
}

struct Running {
    progress: Arc<Progress>,
    rx: mpsc::Receiver<Result<Scan, String>>,
    started: Instant,
}

pub struct App {
    path: String,
    one_volume: bool,
    show_apparent: bool,
    running: Option<Running>,
    scan: Option<Scan>,
    error: Option<String>,
    expanded: HashSet<NodeId>,
    selected: Option<NodeId>,
    /// Folder at the centre of the chart.
    focus: NodeId,
    tab: Tab,
    scroll_to_selected: bool,
    /// Short-lived confirmation shown in the status bar.
    notice: Option<(String, Instant)>,
    /// The path field has keyboard focus, so ⌘⌫ belongs to it.
    editing_path: bool,
    /// `DUW_SCREENSHOT=out.ppm`: save the window once the scan is shown, then quit.
    screenshot: Option<(PathBuf, u32)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<String>) -> Self {
        install_system_fonts(&cc.egui_ctx);
        cc.egui_ctx.all_styles_mut(|s| {
            use egui::TextStyle;
            s.text_styles = [
                (TextStyle::Small, FontId::proportional(SMALL)),
                (TextStyle::Body, FontId::proportional(TEXT)),
                (TextStyle::Button, FontId::proportional(TEXT)),
                (TextStyle::Heading, FontId::proportional(22.0)),
                (TextStyle::Monospace, FontId::monospace(TEXT - 1.0)),
            ]
            .into();
            s.spacing.item_spacing = Vec2::new(8.0, 6.0);
            s.spacing.button_padding = Vec2::new(10.0, 5.0);
            let w = &mut s.visuals.widgets;
            for v in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
                v.rounding = 6.0.into();
            }
            s.visuals.window_rounding = 10.0.into();
            s.visuals.menu_rounding = 8.0.into();
        });
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let mut app = Self {
            path: initial.clone().unwrap_or(home),
            one_volume: true,
            show_apparent: true,
            running: None,
            scan: None,
            error: None,
            expanded: HashSet::new(),
            selected: None,
            focus: Tree::ROOT,
            tab: Tab::Chart,
            scroll_to_selected: false,
            notice: None,
            editing_path: false,
            screenshot: std::env::var_os("DUW_SCREENSHOT").map(|p| (PathBuf::from(p), 0)),
        };
        if initial.is_some() {
            app.start_scan(&cc.egui_ctx);
        }
        app
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        if let Some(r) = &self.running {
            r.progress.cancel.store(true, Ordering::Relaxed);
        }
        let path = expand_tilde(self.path.trim());
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        let (p, ctx2, one_volume) = (progress.clone(), ctx.clone(), self.one_volume);
        std::thread::spawn(move || {
            let _ = tx.send(scan::scan(&path, one_volume, &p));
            ctx2.request_repaint();
        });
        self.running = Some(Running { progress, rx, started: Instant::now() });
        self.error = None;
    }

    fn poll_scan(&mut self, ctx: &egui::Context) {
        let Some(msg) = self.running.as_ref().map(|r| r.rx.try_recv()) else {
            return;
        };
        match msg {
            Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(100)),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.error = Some("The scan stopped unexpectedly".into());
                self.running = None;
            }
            Ok(Ok(scan)) => {
                self.scan = Some(scan);
                self.expanded = HashSet::from([Tree::ROOT]);
                self.selected = None;
                self.focus = Tree::ROOT;
                self.running = None;
            }
            Ok(Err(e)) => {
                let cancelled = self.running.as_ref().unwrap().progress.cancel.load(Ordering::Relaxed);
                if !cancelled {
                    self.error = Some(e);
                }
                self.running = None;
            }
        }
    }

    fn apply(&mut self, ctx: &egui::Context, scan: &mut Scan, actions: Vec<Action>) {
        for action in actions {
            if let Action::Trash(id) = action {
                self.trash(ctx, scan, id);
                continue;
            }
            let tree = &scan.tree;
            match action {
                Action::Select(id) => {
                    self.selected = Some(id);
                    self.focus = chart_focus_for(tree, id);
                }
                Action::Locate(id) => {
                    self.selected = Some(id);
                    for a in tree.ancestors(id) {
                        if a != id {
                            self.expanded.insert(a);
                        }
                    }
                    self.focus = chart_focus_for(tree, id);
                    self.scroll_to_selected = true;
                }
                Action::Toggle(id) => {
                    if !self.expanded.remove(&id) {
                        self.expanded.insert(id);
                    }
                }
                Action::Reveal(id) => {
                    let _ = Command::new("open").arg("-R").arg(tree.path(id)).spawn();
                }
                Action::Open(id) => {
                    let _ = Command::new("open").arg(tree.path(id)).spawn();
                }
                Action::CopyPath(id) => ctx.copy_text(tree.path(id).display().to_string()),
                Action::Trash(_) => unreachable!(),
            }
        }
    }

    /// Move a file or folder (with everything inside) to the Trash, like Finder.
    fn trash(&mut self, ctx: &egui::Context, scan: &mut Scan, id: NodeId) {
        let node = &scan.tree.nodes[id];
        let Some(parent) = node.parent else { return }; // never the scanned folder itself
        if node.removed {
            return;
        }
        let (name, alloc) = (node.name.clone(), node.alloc);
        let path = scan.tree.path(id);
        let out = match Command::new("/usr/bin/trash").arg(&path).output() {
            Ok(out) => out,
            Err(e) => {
                self.error = Some(format!("Couldn't run /usr/bin/trash: {e}"));
                return;
            }
        };
        if !out.status.success() {
            // stderr is an NSLog line; the readable reason is the first quoted part.
            let stderr = String::from_utf8_lossy(&out.stderr);
            let reason = stderr.split('"').nth(1).unwrap_or(stderr.trim());
            self.error = Some(format!("Couldn't move “{name}” to the Trash: {reason}"));
            return;
        }

        scan.remove(id);
        if self.selected.is_some_and(|s| scan.tree.nodes[s].removed) {
            self.selected = Some(parent);
        }
        if scan.tree.nodes[self.focus].removed {
            self.focus = parent;
        }
        self.error = None;
        self.notice = Some((
            format!("Moved “{name}” to the Trash ({}). Empty the Trash to free the space.", fmt_size(alloc)),
            Instant::now(),
        ));
        ctx.request_repaint_after(NOTICE_FOR);
    }

    fn debug_screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, frames)) = &mut self.screenshot else { return };
        if self.scan.is_none() || self.running.is_some() {
            return;
        }
        *frames += 1;
        if *frames == 1 {
            match std::env::var("DUW_SCREENSHOT_THEME").as_deref() {
                Ok("light") => ctx.set_theme(egui::Theme::Light),
                Ok("dark") => ctx.set_theme(egui::Theme::Dark),
                _ => {}
            }
        }
        if *frames == 5 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        }
        ctx.request_repaint();
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = image {
            let mut ppm = format!("P6 {} {} 255\n", img.width(), img.height()).into_bytes();
            for p in &img.pixels {
                ppm.extend_from_slice(&[p.r(), p.g(), p.b()]);
            }
            let _ = std::fs::write(&*path, ppm);
            // Don't let the forced theme leak into the saved preference.
            ctx.set_theme(egui::ThemePreference::System);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    // ---------------------------------------------------------------- toolbar

    fn toolbar(&mut self, ui: &mut egui::Ui) -> bool {
        let mut start = false;
        ui.horizontal(|ui| {
            if ui.button("Choose folder…").clicked() {
                if let Some(p) = rfd::FileDialog::new()
                    .set_directory(expand_tilde(&self.path))
                    .pick_folder()
                {
                    self.path = p.display().to_string();
                    start = true;
                }
            }
            if ui.button("🏠").on_hover_text("Home folder").clicked() {
                self.path = std::env::var("HOME").unwrap_or_else(|_| "/".into());
                start = true;
            }
            // Lay out the right-hand controls first so the path field gets what's left.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut theme = ui.ctx().options(|o| o.theme_preference);
                let before = theme;
                for (value, label) in [
                    (egui::ThemePreference::Dark, "Dark"),
                    (egui::ThemePreference::Light, "Light"),
                    (egui::ThemePreference::System, "System"),
                ] {
                    ui.selectable_value(&mut theme, value, label);
                }
                if theme != before {
                    ui.ctx().set_theme(theme);
                }
                ui.separator();
                ui.checkbox(&mut self.show_apparent, "Apparent size")
                    .on_hover_text("Also show the logical size (what ls -l and Finder's \"Size\" report)");
                ui.checkbox(&mut self.one_volume, "One volume")
                    .on_hover_text("Don't descend into other mounted volumes");
                if let Some(r) = &self.running {
                    if ui.button("⏹  Cancel").clicked() {
                        r.progress.cancel.store(true, Ordering::Relaxed);
                    }
                } else if ui.button("▶  Scan").clicked() {
                    start = true;
                }
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.path)
                        .desired_width(ui.available_width())
                        .hint_text("Folder to scan"),
                );
                self.editing_path = edit.has_focus();
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    start = true;
                }
            });
        });
        start
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(r) = &self.running {
                ui.spinner();
                ui.label(format!(
                    "Scanning…  {} files · {}",
                    fmt_count(r.progress.files.load(Ordering::Relaxed)),
                    fmt_size(r.progress.bytes.load(Ordering::Relaxed)),
                ));
            } else if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, format!("⚠ {e}"));
            } else if let Some((notice, _)) = self.notice.as_ref().filter(|(_, at)| at.elapsed() < NOTICE_FOR) {
                ui.colored_label(Color32::from_rgb(70, 160, 100), format!("✔ {notice}"));
            } else if let Some(scan) = &self.scan {
                let root = &scan.tree.nodes[Tree::ROOT];
                ui.label(RichText::new(format!("{} on disk", fmt_size(root.alloc))).strong());
                ui.label(format!("· {} apparent", fmt_size(root.apparent)));
                ui.label(format!("· {} files", fmt_count(root.files)));
                ui.label(format!("· scanned in {:.1}s", scan.elapsed.as_secs_f32()));
                if scan.unreadable > 0 {
                    ui.label(
                        RichText::new(format!("· ⚠ {} folders unreadable", fmt_count(scan.unreadable as u64)))
                            .color(ui.visuals().warn_fg_color),
                    );
                    if ui
                        .small_button("Grant Full Disk Access…")
                        .on_hover_text("macOS never asks for this: add duw in the list that opens, then rescan")
                        .clicked()
                    {
                        let _ = Command::new("open").arg(FULL_DISK_ACCESS_SETTINGS).spawn();
                    }
                }
            } else {
                ui.label("Choose a folder, or drop one onto the window.");
            }
        });
    }

    // ------------------------------------------------------------------- tree

    fn tree_panel(&self, ui: &mut egui::Ui, scan: &Scan, actions: &mut Vec<Action>) {
        let tree = &scan.tree;
        let mut rows: Vec<(NodeId, usize)> = Vec::new();
        let mut stack = vec![(Tree::ROOT, 0)];
        while let Some((id, depth)) = stack.pop() {
            rows.push((id, depth));
            if self.expanded.contains(&id) {
                stack.extend(tree.nodes[id].children.iter().rev().map(|&c| (c, depth + 1)));
            }
        }

        let mut area = egui::ScrollArea::vertical().auto_shrink(false);
        if self.scroll_to_selected {
            if let Some(i) = rows.iter().position(|r| Some(r.0) == self.selected) {
                let offset = i as f32 * ROW_H - ui.available_height() / 2.0;
                area = area.vertical_scroll_offset(offset.max(0.0));
            }
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        area.show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for &(id, depth) in &rows[range] {
                self.tree_row(ui, tree, id, depth, actions);
            }
        });
    }

    fn tree_row(&self, ui: &mut egui::Ui, tree: &Tree, id: NodeId, depth: usize, actions: &mut Vec<Action>) {
        let node = &tree.nodes[id];
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
        let painter = ui.painter_at(rect);
        let vis = ui.visuals();
        let selected = self.selected == Some(id);
        if selected {
            painter.rect_filled(rect, 5.0, vis.selection.bg_fill);
        } else if resp.hovered() {
            painter.rect_filled(rect, 5.0, vis.widgets.hovered.weak_bg_fill);
        }
        let text = if selected { vis.selection.stroke.color } else { vis.text_color() };
        let weak = if selected { text } else { vis.weak_text_color() };
        let font = FontId::proportional(TEXT);
        let small = FontId::proportional(SMALL);
        let mid = rect.center().y;

        // Right-hand columns, laid out right to left.
        let mut right = rect.right() - 10.0;
        painter.text(Pos2::new(right, mid), Align2::RIGHT_CENTER, fmt_size(node.alloc), font.clone(), text);
        right -= 92.0;
        if self.show_apparent {
            painter.text(Pos2::new(right, mid), Align2::RIGHT_CENTER, fmt_size(node.apparent), small.clone(), weak);
            right -= 88.0;
        }
        let frac = node.parent.map_or(1.0, |p| ratio(node.alloc, tree.nodes[p].alloc));
        let bar = Rect::from_min_size(Pos2::new(right - 64.0, mid - 4.0), Vec2::new(64.0, 8.0));
        painter.rect_filled(bar, 4.0, vis.extreme_bg_color);
        painter.rect_filled(
            Rect::from_min_size(bar.min, Vec2::new((64.0 * frac).max(1.0), 8.0)),
            4.0,
            accent(vis.dark_mode, frac),
        );
        painter.text(
            Pos2::new(bar.left() - 6.0, mid),
            Align2::RIGHT_CENTER,
            format!("{:.1}%", frac * 100.0),
            small.clone(),
            weak,
        );
        let name_right = bar.left() - 60.0;

        // Left: indent, disclosure triangle, icon, name.
        let has_kids = !node.children.is_empty();
        let arrow_x = rect.left() + 10.0 + depth as f32 * 18.0;
        if has_kids {
            let open = self.expanded.contains(&id);
            painter.add(triangle(Pos2::new(arrow_x, mid), open, weak));
        }
        let mut x = arrow_x + 12.0;
        if node.is_dir {
            folder_icon(&painter, x, mid, vis.dark_mode);
        } else {
            file_icon(&painter, x, mid, weak);
        }
        x += 24.0;

        let clip = painter.with_clip_rect(Rect::from_min_max(
            Pos2::new(x, rect.top()),
            Pos2::new(name_right, rect.bottom()),
        ));
        let name = clip.layout_no_wrap(node.name.clone(), font, text);
        let name_w = name.size().x;
        clip.galley(Pos2::new(x, mid - name.size().y / 2.0), name, text);
        x += name_w + 8.0;
        let tag = if node.unreadable {
            Some(("no access", vis.warn_fg_color))
        } else if node.online_only {
            Some(("online only", Color32::from_rgb(90, 140, 210)))
        } else if !node.is_dir && node.is_sparse() {
            Some(("sparse", Color32::from_rgb(90, 170, 120)))
        } else {
            None
        };
        if let Some((label, color)) = tag {
            clip.text(Pos2::new(x, mid), Align2::LEFT_CENTER, label, small, color);
        }

        if resp.clicked() {
            let on_arrow = resp.interact_pointer_pos().is_some_and(|p| p.x < arrow_x + 8.0);
            actions.push(if on_arrow && has_kids { Action::Toggle(id) } else { Action::Select(id) });
        }
        if resp.double_clicked() {
            actions.push(if has_kids { Action::Toggle(id) } else { Action::Open(id) });
        }
        let resp = resp.on_hover_ui_at_pointer(|ui| node_tooltip(ui, tree, id));
        resp.context_menu(|ui| node_menu(ui, tree, id, actions));
    }

    // ------------------------------------------------------------------ chart

    fn chart_view(&self, ui: &mut egui::Ui, scan: &Scan, actions: &mut Vec<Action>) {
        let tree = &scan.tree;
        let focus = self.focus;

        ui.horizontal_wrapped(|ui| {
            for (i, &a) in tree.ancestors(focus).iter().enumerate() {
                if i > 0 {
                    ui.label(RichText::new("›").weak());
                }
                if ui.selectable_label(a == focus, &tree.nodes[a].name).clicked() {
                    actions.push(Action::Locate(a));
                }
            }
        });

        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        let center = rect.center();
        let radius = (rect.width().min(rect.height()) / 2.0 - 14.0).max(60.0);
        let hole = radius * 0.3;
        let ring_w = (radius - hole) / RINGS as f32;

        let mut segs = Vec::new();
        layout_rings(tree, focus, 1, 0.0, TAU, None, &mut segs);

        let hit = |p: Pos2| -> Option<Hit> {
            let d = p - center;
            let r = d.length();
            if r < hole {
                return Some(Hit::Center);
            }
            if r > radius {
                return None;
            }
            let ring = ((r - hole) / ring_w) as usize + 1;
            let mut a = d.x.atan2(-d.y);
            if a < 0.0 {
                a += TAU;
            }
            segs.iter()
                .find(|s| s.ring == ring && a >= s.a0 && a < s.a1)
                .map(|s| Hit::Seg(s.id))
        };
        let hover = resp.hover_pos().and_then(hit);

        let vis = ui.visuals();
        let painter = ui.painter_at(rect);
        for s in &segs {
            let r0 = hole + (s.ring - 1) as f32 * ring_w;
            let hovered = hover == Some(Hit::Seg(s.id));
            let color = seg_color(s, tree.nodes[s.id].is_dir, vis.dark_mode, hovered);
            draw_arc(&painter, center, r0, r0 + ring_w, s.a0, s.a1, color, vis.panel_fill);
        }

        let center_fill = if hover == Some(Hit::Center) && tree.nodes[focus].parent.is_some() {
            vis.widgets.hovered.weak_bg_fill
        } else {
            vis.faint_bg_color
        };
        painter.circle_filled(center, hole - 3.0, center_fill);
        let node = &tree.nodes[focus];
        // Scale the centre label to the hole so it never spills onto the rings.
        let size_pt = (hole * 0.36).clamp(13.0, 24.0);
        let name_pt = (hole * 0.22).clamp(11.0, TEXT);
        let short_name = std::path::Path::new(&node.name)
            .file_name()
            .map_or(node.name.clone(), |n| n.to_string_lossy().into_owned());
        let max_chars = ((hole * 1.6) / (name_pt * 0.55)) as usize;
        painter.text(
            center - Vec2::new(0.0, size_pt * 0.55),
            Align2::CENTER_CENTER,
            ellipsize(&short_name, max_chars.max(4)),
            FontId::proportional(name_pt),
            vis.weak_text_color(),
        );
        painter.text(
            center + Vec2::new(0.0, name_pt * 0.6),
            Align2::CENTER_CENTER,
            fmt_size(node.alloc),
            FontId::proportional(size_pt),
            vis.strong_text_color(),
        );
        if segs.is_empty() {
            painter.text(
                center + Vec2::new(0.0, hole + 24.0),
                Align2::CENTER_CENTER,
                "Nothing inside takes up space",
                FontId::proportional(TEXT),
                vis.weak_text_color(),
            );
        }

        let menu_id = egui::Id::new("chart-menu-target");
        let mut resp = resp;
        match hover {
            Some(Hit::Seg(id)) => {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                resp = resp.on_hover_ui_at_pointer(|ui| node_tooltip(ui, tree, id));
                if resp.clicked() {
                    actions.push(Action::Locate(id));
                }
                if resp.double_clicked() && !tree.nodes[id].is_dir {
                    actions.push(Action::Open(id));
                }
                if resp.secondary_clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(menu_id, id));
                }
            }
            Some(Hit::Center) => {
                if let Some(parent) = node.parent {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    resp = resp.on_hover_text("Go up one level");
                    if resp.clicked() {
                        actions.push(Action::Locate(parent));
                    }
                }
                if resp.secondary_clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(menu_id, focus));
                }
            }
            None => {
                if resp.secondary_clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(menu_id, focus));
                }
            }
        }
        resp.context_menu(|ui| {
            let id = ui.ctx().data(|d| d.get_temp::<NodeId>(menu_id)).unwrap_or(focus);
            node_menu(ui, tree, id, actions);
        });
    }

    // ------------------------------------------------------------ list views

    fn largest_view(&self, ui: &mut egui::Ui, scan: &Scan, actions: &mut Vec<Action>) {
        ui.label(RichText::new("Largest files, by space used on disk. Double-click to reveal in Finder.").weak());
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show_rows(ui, ROW_H, scan.largest.len(), |ui, range| {
                for &id in &scan.largest[range] {
                    self.file_row(ui, &scan.tree, id, actions);
                }
            });
    }

    fn file_row(&self, ui: &mut egui::Ui, tree: &Tree, id: NodeId, actions: &mut Vec<Action>) {
        let node = &tree.nodes[id];
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
        let painter = ui.painter_at(rect);
        let vis = ui.visuals();
        let selected = self.selected == Some(id);
        if selected {
            painter.rect_filled(rect, 5.0, vis.selection.bg_fill);
        } else if resp.hovered() {
            painter.rect_filled(rect, 5.0, vis.widgets.hovered.weak_bg_fill);
        }
        let text = if selected { vis.selection.stroke.color } else { vis.text_color() };
        let weak = if selected { text } else { vis.weak_text_color() };
        let mid = rect.center().y;
        let font = FontId::proportional(TEXT);
        let small = FontId::proportional(SMALL);

        let mut x = rect.left() + 8.0;
        painter.text(Pos2::new(x + 88.0, mid), Align2::RIGHT_CENTER, fmt_size(node.alloc), font.clone(), text);
        x += 98.0;
        if self.show_apparent {
            painter.text(Pos2::new(x + 84.0, mid), Align2::RIGHT_CENTER, fmt_size(node.apparent), small.clone(), weak);
            x += 100.0;
        }
        let name = painter.layout_no_wrap(node.name.clone(), font, text);
        let name_w = name.size().x;
        painter.galley(Pos2::new(x, mid - name.size().y / 2.0), name, text);
        x += name_w + 10.0;
        let folder = node.parent.map(|p| tree.path(p).display().to_string()).unwrap_or_default();
        painter.text(Pos2::new(x, mid), Align2::LEFT_CENTER, folder, small, weak);

        if resp.clicked() {
            actions.push(Action::Locate(id));
        }
        if resp.double_clicked() {
            actions.push(Action::Reveal(id));
        }
        let resp = resp.on_hover_ui_at_pointer(|ui| node_tooltip(ui, tree, id));
        resp.context_menu(|ui| node_menu(ui, tree, id, actions));
    }

    fn types_view(&self, ui: &mut egui::Ui, scan: &Scan) {
        let total = scan.tree.nodes[Tree::ROOT].alloc;
        let max = scan.types.first().map_or(1, |t| t.alloc);
        ui.label(RichText::new("Space used by file extension.").weak());
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show_rows(ui, ROW_H, scan.types.len(), |ui, range| {
                for i in range {
                    let t = &scan.types[i];
                    let color = palette(i, ui.visuals().dark_mode);
                    bar_row(ui, &t.ext, t.alloc, max, total, &format!("{} files", fmt_count(t.count)), color);
                }
            });
    }

    fn ages_view(&self, ui: &mut egui::Ui, scan: &Scan) {
        let total = scan.tree.nodes[Tree::ROOT].alloc;
        let max = scan.ages.iter().map(|b| b.alloc).max().unwrap_or(1);
        ui.label(RichText::new("Space used by when files were last modified.").weak());
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, b) in scan.ages.iter().enumerate() {
            let color = palette(i * 2, ui.visuals().dark_mode);
            bar_row(ui, b.label, b.alloc, max, total, &format!("{} files", fmt_count(b.count)), color);
        }
    }

    fn welcome(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            if let Some(r) = &self.running {
                ui.add(egui::Spinner::new().size(36.0));
                ui.add_space(8.0);
                ui.heading("Scanning…");
                ui.label(format!(
                    "{} files · {} on disk",
                    fmt_count(r.progress.files.load(Ordering::Relaxed)),
                    fmt_size(r.progress.bytes.load(Ordering::Relaxed)),
                ));
                ui.label(RichText::new(format!("{:.0}s", r.started.elapsed().as_secs_f32())).weak());
            } else {
                ui.heading("duw");
                ui.add_space(4.0);
                ui.label("See what is really using your disk.");
                ui.label(
                    RichText::new("Sizes are actual space on disk, so sparse files like VM images count only what they use.")
                        .weak(),
                );
                ui.add_space(12.0);
                ui.label(RichText::new("Choose a folder above, or drop one onto this window.").weak());
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scan(ctx);
        self.debug_screenshot(ctx);

        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        let mut start = false;
        if let Some(p) = dropped {
            self.path = p.display().to_string();
            start = true;
        }

        egui::TopBottomPanel::top("toolbar")
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(12.0, 10.0)))
            .show(ctx, |ui| start |= self.toolbar(ui));
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(12.0, 6.0)))
            .show(ctx, |ui| self.status_bar(ui));

        if start {
            self.start_scan(ctx);
        }

        let Some(scan) = self.scan.take().filter(|_| self.running.is_none()) else {
            egui::CentralPanel::default().show(ctx, |ui| self.welcome(ui));
            return;
        };

        let mut actions = Vec::new();
        egui::SidePanel::left("tree")
            .resizable(true)
            .default_width(ctx.screen_rect().width() * 0.5)
            .min_width(380.0)
            // Always leave room for the chart.
            .max_width((ctx.screen_rect().width() - 360.0).max(380.0))
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::same(6.0)))
            .show(ctx, |ui| self.tree_panel(ui, &scan, &mut actions));
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (tab, label) in [
                    (Tab::Chart, "Chart"),
                    (Tab::Largest, "Largest files"),
                    (Tab::Types, "File types"),
                    (Tab::Ages, "Age"),
                ] {
                    ui.selectable_value(&mut self.tab, tab, label);
                }
            });
            ui.separator();
            match self.tab {
                Tab::Chart => self.chart_view(ui, &scan, &mut actions),
                Tab::Largest => self.largest_view(ui, &scan, &mut actions),
                Tab::Types => self.types_view(ui, &scan),
                Tab::Ages => self.ages_view(ui, &scan),
            }
        });
        // ⌘⌫ trashes the selection, as in Finder (but not while typing a path).
        if let Some(sel) = self.selected.filter(|_| !self.editing_path) {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Backspace)) {
                actions.push(Action::Trash(sel));
            }
        }
        self.scroll_to_selected = false;
        let mut scan = scan;
        self.apply(ctx, &mut scan, actions);
        self.scan = Some(scan);
    }
}

// ------------------------------------------------------------------ helpers

#[derive(PartialEq, Clone, Copy)]
enum Hit {
    Center,
    Seg(NodeId),
}

struct Seg {
    id: NodeId,
    ring: usize,
    a0: f32,
    a1: f32,
    hue: f32,
}

/// Each first-ring slice gets its own hue; descendants drift slightly from it.
fn layout_rings(tree: &Tree, id: NodeId, ring: usize, a0: f32, a1: f32, hue: Option<f32>, segs: &mut Vec<Seg>) {
    let parent = &tree.nodes[id];
    if parent.alloc == 0 {
        return;
    }
    let mut a = a0;
    for (i, &c) in parent.children.iter().enumerate() {
        let span = (a1 - a0) * ratio(tree.nodes[c].alloc, parent.alloc);
        if span < 0.004 {
            break; // children are sorted, so the rest are smaller still
        }
        let mid = (a + span / 2.0 - a0) / (a1 - a0);
        let hue = match hue {
            None => (0.58 + i as f32 * 0.137) % 1.0,
            Some(h) => (h + (mid - 0.5) * 0.08 + 1.0) % 1.0,
        };
        segs.push(Seg { id: c, ring, a0: a, a1: a + span, hue });
        if ring < RINGS && tree.nodes[c].is_dir {
            layout_rings(tree, c, ring + 1, a, a + span, Some(hue), segs);
        }
        a += span;
    }
}

fn draw_arc(p: &egui::Painter, c: Pos2, r0: f32, r1: f32, a0: f32, a1: f32, fill: Color32, gap: Color32) {
    let n = (((a1 - a0) * r1 / 3.0).ceil() as usize).clamp(1, 512);
    let pt = |r: f32, a: f32| c + Vec2::new(a.sin(), -a.cos()) * r;
    let angle = |i: usize| a0 + (a1 - a0) * i as f32 / n as f32;
    let mut mesh = egui::Mesh::default();
    for i in 0..=n {
        mesh.colored_vertex(pt(r0, angle(i)), fill);
        mesh.colored_vertex(pt(r1, angle(i)), fill);
    }
    for i in 0..n as u32 {
        let k = i * 2;
        mesh.add_triangle(k, k + 1, k + 2);
        mesh.add_triangle(k + 1, k + 3, k + 2);
    }
    p.add(Shape::mesh(mesh));
    // Outline in the background colour separates neighbours and smooths edges.
    let outline: Vec<Pos2> = (0..=n)
        .map(|i| pt(r1, angle(i)))
        .chain((0..=n).rev().map(|i| pt(r0, angle(i))))
        .collect();
    p.add(Shape::closed_line(outline, Stroke::new(1.0_f32, gap)));
}

fn seg_color(s: &Seg, is_dir: bool, dark: bool, hovered: bool) -> Color32 {
    let depth = (s.ring - 1) as f32;
    let sat = if is_dir { 0.62 - depth * 0.05 } else { 0.28 };
    let mut val = if dark { 0.86 - depth * 0.08 } else { 0.90 - depth * 0.05 };
    if hovered {
        val = (val + 0.14).min(1.0);
    }
    HsvaGamma { h: s.hue, s: sat, v: val, a: 1.0 }.into()
}

fn palette(i: usize, dark: bool) -> Color32 {
    let hue = (0.58 + i as f32 * 0.137) % 1.0;
    HsvaGamma { h: hue, s: 0.6, v: if dark { 0.85 } else { 0.88 }, a: 1.0 }.into()
}

fn folder_icon(p: &egui::Painter, left: f32, mid: f32, dark: bool) {
    let color: Color32 = HsvaGamma { h: 0.58, s: 0.5, v: if dark { 0.85 } else { 0.75 }, a: 1.0 }.into();
    let tab = Rect::from_min_size(Pos2::new(left, mid - 6.0), Vec2::new(7.0, 4.0));
    let body = Rect::from_min_size(Pos2::new(left, mid - 4.0), Vec2::new(16.0, 11.0));
    p.rect_filled(tab, 1.5, color);
    p.rect_filled(body, 2.0, color);
}

fn file_icon(p: &egui::Painter, left: f32, mid: f32, color: Color32) {
    let body = Rect::from_min_size(Pos2::new(left + 2.0, mid - 7.0), Vec2::new(12.0, 14.0));
    p.rect_stroke(body, 2.0, Stroke::new(1.2_f32, color));
    for dy in [-2.5_f32, 0.5, 3.5] {
        p.line_segment(
            [Pos2::new(left + 5.0, mid + dy), Pos2::new(left + 11.0, mid + dy)],
            Stroke::new(1.0_f32, color),
        );
    }
}

fn accent(dark: bool, frac: f32) -> Color32 {
    // Blue for small shares, shading towards orange for the big ones.
    let hue = 0.6 - 0.52 * frac.clamp(0.0, 1.0);
    HsvaGamma { h: hue, s: 0.6, v: if dark { 0.85 } else { 0.75 }, a: 1.0 }.into()
}

fn triangle(c: Pos2, open: bool, color: Color32) -> Shape {
    let s = 4.0;
    let pts = if open {
        vec![c + Vec2::new(-s, -s * 0.5), c + Vec2::new(s, -s * 0.5), c + Vec2::new(0.0, s * 0.7)]
    } else {
        vec![c + Vec2::new(-s * 0.5, -s), c + Vec2::new(-s * 0.5, s), c + Vec2::new(s * 0.7, 0.0)]
    };
    Shape::convex_polygon(pts, color, Stroke::NONE)
}

fn bar_row(ui: &mut egui::Ui, label: &str, value: u64, max: u64, total: u64, detail: &str, color: Color32) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::hover());
    let painter = ui.painter_at(rect);
    let vis = ui.visuals();
    if resp.hovered() {
        painter.rect_filled(rect, 5.0, vis.widgets.hovered.weak_bg_fill);
    }
    let mid = rect.center().y;
    let font = FontId::proportional(TEXT);
    let small = FontId::proportional(SMALL);
    painter
        .with_clip_rect(Rect::from_min_max(rect.min, Pos2::new(rect.left() + 170.0, rect.bottom())))
        .text(Pos2::new(rect.left() + 8.0, mid), Align2::LEFT_CENTER, label, font.clone(), vis.text_color());
    let bar_l = rect.left() + 180.0;
    let bar_r = (rect.right() - 300.0).max(bar_l + 40.0);
    let track = Rect::from_min_max(Pos2::new(bar_l, mid - 5.0), Pos2::new(bar_r, mid + 5.0));
    painter.rect_filled(track, 5.0, vis.extreme_bg_color);
    let w = (track.width() * ratio(value, max)).max(2.0);
    painter.rect_filled(Rect::from_min_size(track.min, Vec2::new(w, 10.0)), 5.0, color);
    painter.text(Pos2::new(bar_r + 100.0, mid), Align2::RIGHT_CENTER, fmt_size(value), font, vis.text_color());
    painter.text(
        Pos2::new(bar_r + 165.0, mid),
        Align2::RIGHT_CENTER,
        format!("{:.1}%", ratio(value, total) * 100.0),
        small.clone(),
        vis.weak_text_color(),
    );
    painter.text(Pos2::new(rect.right() - 8.0, mid), Align2::RIGHT_CENTER, detail, small, vis.weak_text_color());
}

fn node_tooltip(ui: &mut egui::Ui, tree: &Tree, id: NodeId) {
    let n = &tree.nodes[id];
    ui.label(RichText::new(&n.name).strong());
    ui.label(RichText::new(tree.path(id).display().to_string()).weak().small());
    ui.add_space(4.0);
    egui::Grid::new("tip").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
        ui.label("On disk");
        ui.label(RichText::new(fmt_size(n.alloc)).strong());
        ui.end_row();
        ui.label("Apparent");
        ui.label(fmt_size(n.apparent));
        ui.end_row();
        if let Some(p) = n.parent {
            ui.label("Of parent");
            ui.label(format!("{:.1}%", ratio(n.alloc, tree.nodes[p].alloc) * 100.0));
            ui.end_row();
        }
        if n.is_dir {
            ui.label("Files");
            ui.label(fmt_count(n.files));
            ui.end_row();
        }
        ui.label("Modified");
        ui.label(fmt_age(scan::now_secs() - n.mtime));
        ui.end_row();
    });
    if n.online_only {
        ui.add_space(4.0);
        let what = if n.is_dir { "folder" } else { "file" };
        ui.label(
            RichText::new(format!("Online only: this {what} is in the cloud, not stored on this Mac"))
                .color(Color32::from_rgb(90, 140, 210)),
        );
    } else if n.is_sparse() {
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("Sparse: {} of its logical size isn't stored", fmt_size(n.apparent - n.alloc)))
                .color(Color32::from_rgb(90, 170, 120)),
        );
    }
    if n.unreadable {
        ui.label(RichText::new("Couldn't read this folder").color(ui.visuals().warn_fg_color));
    }
}

fn node_menu(ui: &mut egui::Ui, tree: &Tree, id: NodeId, actions: &mut Vec<Action>) {
    let n = &tree.nodes[id];
    ui.label(RichText::new(ellipsize(&n.name, 40)).strong());
    ui.separator();
    let mut item = |ui: &mut egui::Ui, label: &str, action: Action| {
        if ui.button(label).clicked() {
            actions.push(action);
            ui.close_menu();
        }
    };
    item(ui, "Reveal in Finder", Action::Reveal(id));
    item(ui, if n.is_dir { "Open in Finder" } else { "Open" }, Action::Open(id));
    item(ui, "Show in tree and chart", Action::Locate(id));
    item(ui, "Copy path", Action::CopyPath(id));
    if n.parent.is_some() {
        ui.separator();
        if ui.add(egui::Button::new("Move to Trash").shortcut_text("⌘⌫")).clicked() {
            actions.push(Action::Trash(id));
            ui.close_menu();
        }
    }
}

fn chart_focus_for(tree: &Tree, id: NodeId) -> NodeId {
    if tree.nodes[id].is_dir { id } else { tree.nodes[id].parent.unwrap_or(Tree::ROOT) }
}

fn ratio(a: u64, b: u64) -> f32 {
    if b == 0 { 0.0 } else { (a as f64 / b as f64) as f32 }
}

fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix('~') {
        Some(rest) => PathBuf::from(format!("{}{rest}", std::env::var("HOME").unwrap_or_default())),
        None => PathBuf::from(p),
    }
}

fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

pub fn fmt_size(b: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{b} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn fmt_age(secs: i64) -> String {
    let days = secs / 86_400;
    match days {
        i64::MIN..=0 => "today".into(),
        1 => "yesterday".into(),
        2..=59 => format!("{days} days ago"),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}
