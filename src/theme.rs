use eframe::egui::{self, Color32, FontFamily, FontId, Margin, RichText, Stroke, TextStyle};

pub const BG: Color32 = Color32::from_rgb(14, 20, 30);
pub const PANEL: Color32 = Color32::from_rgb(23, 34, 46);
pub const EDGE: Color32 = Color32::from_rgb(49, 73, 84);
pub const MINT: Color32 = Color32::from_rgb(91, 234, 173);
pub const TEXT: Color32 = Color32::from_rgb(222, 235, 231);
pub const MUTED: Color32 = Color32::from_rgb(143, 163, 171);
pub const GOLD: Color32 = Color32::from_rgb(244, 200, 111);

pub fn install(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = PANEL;
    style.visuals.faint_bg_color = PANEL;
    style.visuals.extreme_bg_color = BG;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.window_corner_radius = 0.into();
    style.visuals.window_stroke = Stroke::new(2.0, EDGE);
    style.visuals.selection.bg_fill = Color32::from_rgb(35, 89, 72);
    style.visuals.selection.stroke = Stroke::new(1.0, MINT);
    for widget in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.corner_radius = 0.into();
        widget.bg_stroke = Stroke::new(1.0, EDGE);
    }
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(32, 48, 61);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(43, 73, 71);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, MINT);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(36, 94, 70);
    style.visuals.widgets.inactive.weak_bg_fill = style.visuals.widgets.inactive.bg_fill;
    style.visuals.widgets.hovered.weak_bg_fill = style.visuals.widgets.hovered.bg_fill;
    style.visuals.widgets.active.weak_bg_fill = style.visuals.widgets.active.bg_fill;
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.spacing.interact_size.y = 32.0;
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(14.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::new(20.0, FontFamily::Monospace));
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    ctx.set_style(style);
}

pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, EDGE))
        .corner_radius(0)
        .inner_margin(Margin::same(18))
}

pub fn heading(ui: &mut egui::Ui, number: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(number).monospace().color(MINT).size(15.0));
        ui.label(RichText::new(title).strong().size(17.0));
    });
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(3.0);
}
