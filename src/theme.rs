//! Color themes. Keys follow the theme JSON format (`AppContentBackground`, `EditorKeywordText`, …)
//! so their theme files import as-is; `Strata*` keys add what Strata draws beyond that.

use egui::{Color32, CornerRadius, Stroke, Visuals};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub name: String,
    pub dark: bool,
    pub window: Color32,
    pub content: Color32,
    pub sidebar: Color32,
    pub rail: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub border: Color32,
    pub separator: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub tab_selected: Color32,
    pub grid_header: Color32,
    pub grid_header_text: Color32,
    pub grid_even: Color32,
    pub grid_odd: Color32,
    pub grid_text: Color32,
    pub grid_number: Color32,
    pub grid_placeholder: Color32,
    pub grid_separator: Color32,
    pub grid_selection: Color32,
    pub grid_selection_border: Color32,
    pub modified: Color32,
    pub inserted: Color32,
    pub deleted: Color32,
    pub editor_bg: Color32,
    pub editor_text: Color32,
    pub editor_keyword: Color32,
    pub editor_function: Color32,
    pub editor_string: Color32,
    pub editor_dquote: Color32,
    pub editor_backtick: Color32,
    pub editor_number: Color32,
    pub editor_comment: Color32,
    pub editor_current_query: Color32,
    pub editor_selection: Color32,
    pub chart: [Color32; 5],
    /// Window ground behind the floating panels (translucent when the OS blurs the window).
    pub ground: Color32,
    /// Floating chrome: rail, command bar, explorer, inspector, docks.
    pub glass: Color32,
    pub glass_stroke: Color32,
    /// Secondary accent for links, filters, foreign keys and selection.
    pub accent2: Color32,
}

pub fn hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(s, 16).ok()?;
    match s.len() {
        6 => Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)),
        8 => Some(Color32::from_rgba_unmultiplied((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8)),
        _ => None,
    }
}

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

pub fn luminance(c: Color32) -> f32 {
    (0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32) / 255.0
}

impl Palette {
    /// Builds a palette from theme JSON keys over a base palette.
    pub fn from_keys(name: &str, base: &Palette, keys: &BTreeMap<String, String>) -> Palette {
        let mut p = base.clone();
        p.name = name.to_string();
        let get = |k: &str| keys.get(k).and_then(|v| hex(v));
        macro_rules! set {
            ($field:ident, $($key:literal),+) => {
                $( if let Some(c) = get($key) { p.$field = c; } )+
            };
        }
        set!(window, "AppWindowBackground");
        set!(content, "AppContentBackground");
        set!(sidebar, "AppSidebarBackground");
        set!(surface, "AppControlBackground");
        set!(border, "AppControlBorder");
        set!(separator, "AppSeparator");
        set!(tab_selected, "AppTabSelectedBackground");
        set!(surface_hover, "AppTabHoverBackground");
        set!(text, "AppQueryMessageText", "GridText");
        set!(text_dim, "AppTabUnselectedText");
        set!(success, "AppNotificationSuccessBackground");
        set!(warning, "AppNotificationWarningBackground");
        set!(danger, "AppNotificationErrorBackground");
        set!(grid_header, "GridHeaderBackground");
        set!(grid_header_text, "GridHeaderText");
        set!(grid_even, "GridEvenRowBackground");
        set!(grid_odd, "GridOddRowBackground");
        set!(grid_text, "GridText");
        set!(grid_number, "GridNumberText");
        set!(grid_placeholder, "GridPlaceholderText");
        set!(grid_separator, "GridSeparator");
        set!(grid_selection_border, "GridCellSelectionBorder");
        set!(modified, "ContentModifiedBackground");
        set!(inserted, "ContentInsertedBackground");
        set!(deleted, "ContentDeletedRowBackground");
        set!(editor_text, "EditorText");
        set!(editor_keyword, "EditorKeywordText");
        set!(editor_function, "EditorFunctionText");
        set!(editor_string, "EditorSingleQuotedText");
        set!(editor_dquote, "EditorDoubleQuotedText");
        set!(editor_backtick, "EditorBacktickQuotedText");
        set!(editor_number, "EditorNumberText");
        set!(editor_comment, "EditorCommentText");
        set!(editor_current_query, "EditorCurrentQueryBackground");
        set!(editor_selection, "EditorSelectionBackground");
        set!(rail, "StrataRail");
        set!(accent, "StrataAccent");
        set!(text_faint, "StrataTextFaint");
        for (i, key) in ["AppChartPaletteColor1", "AppChartPaletteColor2", "AppChartPaletteColor3", "AppChartPaletteColor4", "AppChartPaletteColor5"].iter().enumerate() {
            if let Some(c) = get(key) {
                p.chart[i] = c;
            }
        }
        if !keys.contains_key("StrataRail") {
            p.rail = mix(p.sidebar, Color32::BLACK, if p.dark { 0.25 } else { 0.04 });
        }
        if !keys.contains_key("EditorSelectionBackground") && keys.contains_key("StrataAccent") {
            p.editor_selection = mix(p.content, p.accent, 0.35);
        }
        set!(accent2, "StrataAccent2");
        p.editor_bg = p.content;
        p.on_accent = if luminance(p.accent) > 0.6 { Color32::from_rgb(10, 14, 20) } else { Color32::WHITE };
        if !keys.contains_key("StrataGround") {
            p.ground = p.window;
        }
        if !keys.contains_key("StrataGlass") {
            let [r, g, b, _] = mix(p.sidebar, if p.dark { Color32::WHITE } else { Color32::WHITE }, if p.dark { 0.04 } else { 0.5 }).to_array();
            p.glass = Color32::from_rgba_unmultiplied(r, g, b, if p.dark { 200 } else { 196 });
        }
        if let Some(c) = get("StrataGround") {
            p.ground = c;
        }
        if let Some(c) = get("StrataGlass") {
            p.glass = c;
        }
        p
    }

    /// Strata's default: an ink ground, glass chrome, amber and mineral-cyan accents.
    pub fn night() -> Palette {
        Palette {
            name: "Strata Night".into(),
            dark: true,
            window: hex("#0B0E13").unwrap(),
            content: hex("#12161C").unwrap(),
            sidebar: hex("#181E27").unwrap(),
            rail: hex("#141920").unwrap(),
            surface: hex("#1A2029").unwrap(),
            surface_hover: hex("#232B36").unwrap(),
            border: hex("#2A323D").unwrap(),
            separator: hex("#1F262F").unwrap(),
            text: hex("#E8ECF1").unwrap(),
            text_dim: hex("#A3ADBA").unwrap(),
            text_faint: hex("#7D8794").unwrap(),
            accent: hex("#F2B252").unwrap(),
            on_accent: hex("#1A1308").unwrap(),
            success: hex("#5BD69A").unwrap(),
            warning: hex("#F2B252").unwrap(),
            danger: hex("#FF6B5B").unwrap(),
            tab_selected: hex("#232B36").unwrap(),
            grid_header: hex("#151A21").unwrap(),
            grid_header_text: hex("#F4F6F9").unwrap(),
            grid_even: hex("#12161C").unwrap(),
            grid_odd: hex("#141920").unwrap(),
            grid_text: hex("#E2E7ED").unwrap(),
            grid_number: hex("#BCEBF5").unwrap(),
            grid_placeholder: hex("#7D8794").unwrap(),
            grid_separator: hex("#1C222A").unwrap(),
            grid_selection: hex("#173039").unwrap(),
            grid_selection_border: hex("#5CC8E0").unwrap(),
            modified: hex("#3A3020").unwrap(),
            inserted: hex("#163226").unwrap(),
            deleted: hex("#3A1F1F").unwrap(),
            editor_bg: hex("#12161C").unwrap(),
            editor_text: hex("#E2E7ED").unwrap(),
            editor_keyword: hex("#F2B252").unwrap(),
            editor_function: hex("#5CC8E0").unwrap(),
            editor_string: hex("#B9E58C").unwrap(),
            editor_dquote: hex("#F0A6CA").unwrap(),
            editor_backtick: hex("#F7C77A").unwrap(),
            editor_number: hex("#F59E8B").unwrap(),
            editor_comment: hex("#7D8794").unwrap(),
            editor_current_query: hex("#161B22").unwrap(),
            editor_selection: hex("#2A3D4C").unwrap(),
            chart: [hex("#5CC8E0").unwrap(), hex("#F2B252").unwrap(), hex("#5BD69A").unwrap(), hex("#FF8A7E").unwrap(), hex("#8FB4D8").unwrap()],
            ground: Color32::from_rgba_unmultiplied(11, 14, 19, 236),
            glass: hex("#181E27").unwrap(),
            glass_stroke: Color32::from_rgba_unmultiplied(255, 255, 255, 24),
            accent2: hex("#5CC8E0").unwrap(),
        }
    }

    pub fn day() -> Palette {
        Palette {
            name: "Strata Day".into(),
            dark: false,
            window: hex("#E9EDF2").unwrap(),
            content: hex("#FFFFFF").unwrap(),
            sidebar: hex("#F6F8FA").unwrap(),
            rail: hex("#F1F4F7").unwrap(),
            surface: hex("#FFFFFF").unwrap(),
            surface_hover: hex("#EDF1F5").unwrap(),
            border: hex("#D5DBE3").unwrap(),
            separator: hex("#E4E8EE").unwrap(),
            text: hex("#141A22").unwrap(),
            text_dim: hex("#4E5A6A").unwrap(),
            text_faint: hex("#677180").unwrap(),
            accent: hex("#A35C00").unwrap(),
            on_accent: Color32::WHITE,
            success: hex("#0F7A4B").unwrap(),
            warning: hex("#A35C00").unwrap(),
            danger: hex("#C2352A").unwrap(),
            tab_selected: hex("#FFFFFF").unwrap(),
            grid_header: hex("#F6F8FA").unwrap(),
            grid_header_text: hex("#141A22").unwrap(),
            grid_even: hex("#FFFFFF").unwrap(),
            grid_odd: hex("#F8FAFB").unwrap(),
            grid_text: hex("#1B232D").unwrap(),
            grid_number: hex("#0E5E75").unwrap(),
            grid_placeholder: hex("#7A8494").unwrap(),
            grid_separator: hex("#EBEEF2").unwrap(),
            grid_selection: hex("#DCF1F6").unwrap(),
            grid_selection_border: hex("#0E7490").unwrap(),
            modified: hex("#FCEFD6").unwrap(),
            inserted: hex("#DDF4E7").unwrap(),
            deleted: hex("#FBE1DE").unwrap(),
            editor_bg: hex("#FFFFFF").unwrap(),
            editor_text: hex("#1B232D").unwrap(),
            editor_keyword: hex("#A35C00").unwrap(),
            editor_function: hex("#0E7490").unwrap(),
            editor_string: hex("#2F7D1F").unwrap(),
            editor_dquote: hex("#B42D72").unwrap(),
            editor_backtick: hex("#8A5A00").unwrap(),
            editor_number: hex("#B9381F").unwrap(),
            editor_comment: hex("#7A8494").unwrap(),
            editor_current_query: hex("#F7F9FB").unwrap(),
            editor_selection: hex("#CDE9F1").unwrap(),
            chart: [hex("#0E7490").unwrap(), hex("#C27A0E").unwrap(), hex("#0F7A4B").unwrap(), hex("#C2352A").unwrap(), hex("#5B6B8C").unwrap()],
            ground: Color32::from_rgba_unmultiplied(233, 237, 242, 230),
            glass: hex("#FFFFFF").unwrap(),
            glass_stroke: Color32::from_rgba_unmultiplied(20, 30, 45, 26),
            accent2: hex("#0E7490").unwrap(),
        }
    }

    pub fn builtins() -> Vec<Palette> {
        let night = Palette::night();
        let day = Palette::day();
        let k = |pairs: &[(&str, &str)]| pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<BTreeMap<_, _>>();
        let jade = Palette::from_keys(
            "Jade",
            &night,
            &k(&[
                ("AppWindowBackground", "#0B2127"),
                ("AppContentBackground", "#002B36"),
                ("AppSidebarBackground", "#00212B"),
                ("AppControlBackground", "#003440"),
                ("AppControlBorder", "#1E4550"),
                ("AppTabSelectedBackground", "#0F4A5B"),
                ("AppTabHoverBackground", "#005167"),
                ("GridHeaderBackground", "#002B36"),
                ("GridEvenRowBackground", "#002B36"),
                ("GridOddRowBackground", "#04313D"),
                ("GridSeparator", "#0F3E4A"),
                ("EditorKeywordText", "#529BD8"),
                ("EditorFunctionText", "#7CB342"),
                ("EditorCommentText", "#2E8B57"),
                ("EditorNumberText", "#B57AFF"),
                ("EditorSingleQuotedText", "#E0A07E"),
                ("EditorCurrentQueryBackground", "#003745"),
                ("StrataAccent", "#2BB3A3"),
            ]),
        );
        let graphite = Palette::from_keys(
            "Graphite",
            &night,
            &k(&[
                ("AppWindowBackground", "#161616"),
                ("AppContentBackground", "#1B1B1B"),
                ("AppSidebarBackground", "#141414"),
                ("AppControlBackground", "#232323"),
                ("AppControlBorder", "#323232"),
                ("AppTabSelectedBackground", "#2A2A2A"),
                ("AppTabHoverBackground", "#2C2C2C"),
                ("GridHeaderBackground", "#1F1F1F"),
                ("GridEvenRowBackground", "#1B1B1B"),
                ("GridOddRowBackground", "#202020"),
                ("GridSeparator", "#2A2A2A"),
                ("EditorCurrentQueryBackground", "#202020"),
                ("StrataAccent", "#FF8A3D"),
            ]),
        );
        let dusk = Palette::from_keys(
            "Dusk",
            &night,
            &k(&[
                ("AppWindowBackground", "#17121F"),
                ("AppContentBackground", "#1B1524"),
                ("AppSidebarBackground", "#140F1B"),
                ("AppControlBackground", "#241C31"),
                ("AppControlBorder", "#352A47"),
                ("AppTabSelectedBackground", "#2B2140"),
                ("AppTabHoverBackground", "#2E2442"),
                ("GridHeaderBackground", "#1F1829"),
                ("GridEvenRowBackground", "#1B1524"),
                ("GridOddRowBackground", "#1F182A"),
                ("GridSeparator", "#2A2138"),
                ("EditorCurrentQueryBackground", "#21192D"),
                ("StrataAccent", "#E879F9"),
            ]),
        );
        let paper = Palette::from_keys(
            "Paper",
            &day,
            &k(&[
                ("AppWindowBackground", "#F3EFE6"),
                ("AppContentBackground", "#FBF8F1"),
                ("AppSidebarBackground", "#EFEADF"),
                ("AppControlBackground", "#FFFDF8"),
                ("AppControlBorder", "#DDD5C4"),
                ("GridHeaderBackground", "#F4EFE4"),
                ("GridEvenRowBackground", "#FBF8F1"),
                ("GridOddRowBackground", "#F6F2E9"),
                ("GridSeparator", "#EAE3D5"),
                ("EditorCurrentQueryBackground", "#F4EFE4"),
                ("StrataAccent", "#B4541B"),
            ]),
        );
        vec![night, day, jade, graphite, dusk, paper]
    }

    /// Reads a theme JSON file (`{"name", "base", "colors": {...}}`).
    pub fn load_file(path: &std::path::Path) -> anyhow::Result<Palette> {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let name = v["name"].as_str().unwrap_or("Imported").to_string();
        let base = if v["base"].as_str() == Some("light") { Palette::day() } else { Palette::night() };
        let colors: BTreeMap<String, String> = v["colors"]
            .as_object()
            .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
            .unwrap_or_default();
        if colors.is_empty() {
            anyhow::bail!("No colors in theme file");
        }
        Ok(Palette::from_keys(&name, &base, &colors))
    }

    /// Floating sheets use an opaque fill so underlying rows never bleed through menus or dialogs.
    pub fn sheet(&self) -> Color32 {
        let [r, g, b, _] = mix(self.glass_opaque(), self.surface, 0.35).to_array();
        Color32::from_rgb(r, g, b)
    }

    /// The glass tone without its transparency.
    pub fn glass_opaque(&self) -> Color32 {
        let [r, g, b, a] = self.glass.to_srgba_unmultiplied();
        let _ = a;
        Color32::from_rgb(r, g, b)
    }

    pub fn apply(&self, ctx: &egui::Context, scale: f32) {
        let mut v = if self.dark { Visuals::dark() } else { Visuals::light() };
        let r = CornerRadius::same(8);
        v.override_text_color = Some(self.text);
        // Panels paint their own glass or solid cards; the default fill would cover the ground.
        v.panel_fill = Color32::TRANSPARENT;
        v.window_fill = self.sheet();
        v.extreme_bg_color = if self.dark { mix(self.content, Color32::BLACK, 0.25) } else { mix(self.content, self.window, 0.4) };
        v.faint_bg_color = self.grid_odd;
        v.code_bg_color = self.surface_hover;
        v.window_stroke = Stroke::new(1.0, self.glass_stroke);
        v.window_corner_radius = CornerRadius::same(20);
        v.menu_corner_radius = CornerRadius::same(14);
        v.hyperlink_color = self.accent2;
        v.selection.bg_fill = mix(self.content, self.accent2, if self.dark { 0.35 } else { 0.25 });
        v.selection.stroke = Stroke::new(1.0, self.accent2);
        v.warn_fg_color = self.warning;
        v.error_fg_color = self.danger;
        v.text_cursor.stroke = Stroke::new(2.0, self.accent);
        v.striped = true;
        v.widgets.noninteractive.bg_fill = self.surface;
        v.widgets.noninteractive.weak_bg_fill = self.surface;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.separator);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.text);
        v.widgets.noninteractive.corner_radius = r;
        v.widgets.inactive.bg_fill = self.surface;
        v.widgets.inactive.weak_bg_fill = self.surface;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, self.border);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, self.text);
        v.widgets.inactive.corner_radius = r;
        v.widgets.hovered.bg_fill = self.surface_hover;
        v.widgets.hovered.weak_bg_fill = self.surface_hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, mix(self.border, self.text, 0.25));
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, self.text);
        v.widgets.hovered.corner_radius = r;
        v.widgets.hovered.expansion = 0.0;
        v.widgets.active.bg_fill = mix(self.surface_hover, self.accent, 0.22);
        v.widgets.active.weak_bg_fill = mix(self.surface_hover, self.accent, 0.22);
        v.widgets.active.bg_stroke = Stroke::new(1.0, self.accent);
        v.widgets.active.fg_stroke = Stroke::new(1.0, self.text);
        v.widgets.active.corner_radius = r;
        v.widgets.active.expansion = 0.0;
        v.widgets.open = v.widgets.active;
        v.widgets.open.bg_fill = self.surface_hover;
        v.widgets.open.weak_bg_fill = self.surface_hover;
        v.popup_shadow = egui::Shadow { offset: [0, 12], blur: 32, spread: 0, color: Color32::from_black_alpha(if self.dark { 140 } else { 46 }) };
        v.window_shadow = egui::Shadow { offset: [0, 24], blur: 64, spread: 0, color: Color32::from_black_alpha(if self.dark { 150 } else { 56 }) };
        ctx.set_theme(if self.dark { egui::Theme::Dark } else { egui::Theme::Light });
        ctx.set_visuals(v);
        ctx.all_styles_mut(|s| {
            s.spacing.item_spacing = egui::vec2(8.0, 6.0);
            s.spacing.menu_margin = egui::Margin::same(8);
            s.spacing.window_margin = egui::Margin::same(20);
            s.spacing.button_padding = egui::vec2(12.0, 6.0);
            s.spacing.interact_size.y = 30.0;
            s.spacing.scroll.bar_width = 8.0;
            s.spacing.scroll.floating = true;
            s.interaction.selectable_labels = false;
        });
        ctx.set_zoom_factor(scale);
    }
}
