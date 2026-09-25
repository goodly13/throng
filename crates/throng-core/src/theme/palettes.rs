//! The built-in themes converted from palettes published by others. Each keeps its source's
//! grounds, text and accent; the rest follows throng's derivation ([`super::make`]): syntax hues
//! are lifted to 6:1 on the editor, search surfaces are tinted only as far as code still reads, and
//! a palette's terminal colours are lifted to 4.5:1 on the terminal's ground. Where a source has no
//! colour for a token (a muted text that reads at 4.5:1, say), the nearest step was chosen and is
//! marked "derived". Colour values are facts about a palette; each source is credited beside it,
//! and those under the MIT licence are also listed in `NOTICE`.

use super::{P, Palette};

#[rustfmt::skip]
pub(super) const PALETTES: &[(&str, Palette)] = &[
    // Darkmatter, a 21st.dev community theme by serafimcloud
    // (https://21st.dev/community/themes/darkmatter): its dark variant's tokens, with its chart
    // colours as the syntax hues.
    ("Darkmatter", Palette {
        bg: "#121113", sidebar: Some("#121212"), surface: "#1c1b1d", surface_active: Some("#333333"),
        text: "#c1c1c1", text_muted: Some("#888888"), accent: "#e78a53", border: Some("#262626"),
        status_bar: Some("#121212"), terminal_bg: Some("#121113"), selection: Some("#3a2a20"),
        unsaved: Some("#fbcb97"),
        syntax: ["#e78a53", "#5f8787", "#888888", "#fbcb97", "#87afaf", "#f0a878", "#c1c1c1", "#999999", "#999999", "#e5534b"],
        ..P
    }),
    // Slate Dark, a 21st.dev community theme by soulrenderlive-design
    // (https://21st.dev/community/themes/slate-dark-1789202367405): neutral greys, crimson accent.
    ("Slate Dark", Palette {
        bg: "#1a1a1a", sidebar: Some("#141414"), surface: "#1f1f1f", surface_active: Some("#333333"),
        text: "#fafafa", text_muted: Some("#a6a6a6"), accent: "#e6003d", danger: Some("#e5484d"),
        success: Some("#36c9b4"), border: Some("#333333"), status_bar: Some("#141414"),
        selection: Some("#4d0014"), unsaved: Some("#f0d38a"),
        syntax: ["#e6003d", "#36c9b4", "#8c8c8c", "#f7bd8a", "#f0d38a", "#ee9a82", "#fafafa", "#c9c9c9", "#a6a6a6", "#ff4d4d"],
        ..P
    }),
    // Amber Dark, a 21st.dev community theme by vanweerenbern
    // (https://21st.dev/community/themes/amber-dark-1788951724951): near-black zinc with its amber
    // ring as the accent. Its chart is all ambers, so the other syntax hues are derived.
    ("Amber Dark", Palette {
        bg: "#09090b", sidebar: Some("#101013"), surface: "#17171b", surface_active: Some("#1c1c21"),
        text: "#fafafa", text_muted: Some("#a1a1aa"), accent: "#e3af35", border: Some("#222226"),
        status_bar: Some("#101013"), selection: Some("#3d2e0e"), unsaved: Some("#c2820a"),
        syntax: ["#e3af35", "#b5d18a", "#71717a", "#f0a64b", "#7cc4d8", "#f4d58d", "#e4e4e7", "#a1a1aa", "#a1a1aa", "#f87171"],
        ..P
    }),
    // Violet Dark, a 21st.dev community theme by ryan_d38f796e
    // (https://21st.dev/community/themes/violet-dark-1789799839906): neon on black. Its muted text
    // is the sidebar's #888888, since #777777 reads under 4.5:1.
    ("Violet Dark", Palette {
        bg: "#050505", sidebar: Some("#020202"), surface: "#0d0d0d", surface_active: Some("#1a1a1a"),
        text: "#00ff9f", text_muted: Some("#888888"), accent: "#bc13fe", danger: Some("#ff0055"),
        success: Some("#00ff9f"), border: Some("#333333"), status_bar: Some("#020202"),
        editor_fg: Some("#d0ffe9"), selection: Some("#2a0640"), unsaved: Some("#f7ff00"),
        syntax: ["#bc13fe", "#f7ff00", "#777777", "#ff0055", "#00b8ff", "#00ff9f", "#d0ffe9", "#bc13fe", "#888888", "#ff0055"],
        ..P
    }),
    // Tokyo Night, by enkia (https://github.com/enkia/tokyo-night-vscode-theme), MIT: the Night
    // variant.
    ("Tokyo Night", Palette {
        bg: "#1a1b26", sidebar: Some("#16161e"), surface: "#1f2335", surface_active: Some("#292e42"),
        text: "#c0caf5", text_muted: Some("#9aa5ce"), accent: "#7aa2f7", danger: Some("#f7768e"),
        success: Some("#9ece6a"), border: Some("#292e42"), status_bar: Some("#16161e"),
        selection: Some("#33467c"), unsaved: Some("#e0af68"),
        syntax: ["#bb9af7", "#9ece6a", "#565f89", "#ff9e64", "#2ac3de", "#7aa2f7", "#c0caf5", "#89ddff", "#a9b1d6", "#f7768e"],
        ansi: Some(["#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
                    "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5"]),
        ..P
    }),
    // Catppuccin Mocha, by the Catppuccin organisation (https://github.com/catppuccin/catppuccin),
    // MIT.
    ("Catppuccin Mocha", Palette {
        bg: "#1e1e2e", sidebar: Some("#181825"), surface: "#313244", surface_active: Some("#45475a"),
        text: "#cdd6f4", text_muted: Some("#a6adc8"), accent: "#cba6f7", danger: Some("#f38ba8"),
        success: Some("#a6e3a1"), border: Some("#45475a"), status_bar: Some("#181825"),
        selection: Some("#45475a"), unsaved: Some("#f9e2af"),
        syntax: ["#cba6f7", "#a6e3a1", "#9399b2", "#fab387", "#f9e2af", "#89b4fa", "#cdd6f4", "#89dceb", "#9399b2", "#f38ba8"],
        ansi: Some(["#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
                    "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8"]),
        ..P
    }),
    // Catppuccin Latte, the light flavour of the same palette, MIT.
    ("Catppuccin Latte", Palette {
        bg: "#eff1f5", sidebar: Some("#e6e9ef"), surface: "#ffffff", surface_active: Some("#ccd0da"),
        text: "#4c4f69", text_muted: Some("#5c5f77"), accent: "#8839ef", danger: Some("#d20f39"),
        success: Some("#40a02b"), border: Some("#bcc0cc"), status_bar: Some("#dce0e8"),
        terminal_bg: Some("#eff1f5"), editor_bg: Some("#eff1f5"), editor_fg: Some("#4c4f69"),
        selection: Some("#d4c8f5"), unsaved: Some("#df8e1d"),
        syntax: ["#8839ef", "#40a02b", "#7c7f93", "#fe640b", "#df8e1d", "#1e66f5", "#4c4f69", "#04a5e5", "#7c7f93", "#d20f39"],
        ansi: Some(["#5c5f77", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#acb0be",
                    "#6c6f85", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#bcc0cc"]),
        ..P
    }),
    // Nord, by Arctic Ice Studio (https://github.com/nordtheme/nord), MIT. The sidebar and muted
    // text are derived: Nord has no step between nord0 and nord3 that reads.
    ("Nord", Palette {
        bg: "#2e3440", sidebar: Some("#292e39"), surface: "#3b4252", surface_active: Some("#434c5e"),
        text: "#d8dee9", text_muted: Some("#a3abbb"), accent: "#88c0d0", danger: Some("#bf616a"),
        success: Some("#a3be8c"), border: Some("#434c5e"), status_bar: Some("#3b4252"),
        selection: Some("#434c5e"), unsaved: Some("#ebcb8b"),
        syntax: ["#81a1c1", "#a3be8c", "#616e88", "#b48ead", "#8fbcbb", "#88c0d0", "#d8dee9", "#81a1c1", "#eceff4", "#bf616a"],
        ansi: Some(["#3b4252", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#88c0d0", "#e5e9f0",
                    "#4c566a", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#8fbcbb", "#eceff4"]),
        ..P
    }),
    // Dracula, by Zeno Rocha and contributors (https://github.com/dracula/dracula-theme), MIT.
    ("Dracula", Palette {
        bg: "#282a36", sidebar: Some("#21222c"), surface: "#343746", surface_active: Some("#44475a"),
        text: "#f8f8f2", text_muted: Some("#a8adcc"), accent: "#bd93f9", danger: Some("#ff5555"),
        success: Some("#50fa7b"), border: Some("#44475a"), status_bar: Some("#191a21"),
        selection: Some("#44475a"), unsaved: Some("#f1fa8c"),
        syntax: ["#ff79c6", "#f1fa8c", "#6272a4", "#bd93f9", "#8be9fd", "#50fa7b", "#f8f8f2", "#ff79c6", "#f8f8f2", "#ff5555"],
        ansi: Some(["#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
                    "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff"]),
        ..P
    }),
    // Gruvbox Dark, by Pavel Pertsev (https://github.com/morhetz/gruvbox), MIT.
    ("Gruvbox Dark", Palette {
        bg: "#282828", sidebar: Some("#1d2021"), surface: "#3c3836", surface_active: Some("#504945"),
        text: "#ebdbb2", text_muted: Some("#a89984"), accent: "#fe8019", danger: Some("#fb4934"),
        success: Some("#b8bb26"), border: Some("#504945"), status_bar: Some("#1d2021"),
        selection: Some("#504945"), unsaved: Some("#fabd2f"),
        syntax: ["#fb4934", "#b8bb26", "#928374", "#d3869b", "#fabd2f", "#8ec07c", "#83a598", "#fe8019", "#a89984", "#cc241d"],
        ansi: Some(["#282828", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#a89984",
                    "#928374", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b", "#8ec07c", "#ebdbb2"]),
        ..P
    }),
    // Rose Pine, by the Rose Pine organisation (https://github.com/rose-pine/rose-pine-theme), MIT.
    // It has no green; its foam stands in for success.
    ("Ros\u{e9} Pine", Palette {
        bg: "#191724", sidebar: Some("#1f1d2e"), surface: "#26233a", surface_active: Some("#403d52"),
        text: "#e0def4", text_muted: Some("#908caa"), accent: "#c4a7e7", danger: Some("#eb6f92"),
        success: Some("#9ccfd8"), border: Some("#403d52"), status_bar: Some("#1f1d2e"),
        selection: Some("#403d52"), unsaved: Some("#f6c177"),
        syntax: ["#31748f", "#f6c177", "#6e6a86", "#c4a7e7", "#9ccfd8", "#ebbcba", "#e0def4", "#908caa", "#908caa", "#eb6f92"],
        ansi: Some(["#26233a", "#eb6f92", "#31748f", "#f6c177", "#9ccfd8", "#c4a7e7", "#ebbcba", "#e0def4",
                    "#6e6a86", "#eb6f92", "#31748f", "#f6c177", "#9ccfd8", "#c4a7e7", "#ebbcba", "#e0def4"]),
        ..P
    }),
    // One Dark, from Atom's One Dark syntax theme (https://github.com/atom/one-dark-syntax), MIT.
    ("One Dark", Palette {
        bg: "#282c34", sidebar: Some("#21252b"), surface: "#2c313a", surface_active: Some("#3e4451"),
        text: "#abb2bf", text_muted: Some("#8b929e"), accent: "#61afef", danger: Some("#e06c75"),
        success: Some("#98c379"), border: Some("#3a3f4b"), status_bar: Some("#21252b"),
        selection: Some("#3e4451"), unsaved: Some("#e5c07b"),
        syntax: ["#c678dd", "#98c379", "#5c6370", "#d19a66", "#e5c07b", "#61afef", "#e06c75", "#56b6c2", "#abb2bf", "#ff616e"],
        ansi: Some(["#282c34", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
                    "#5c6370", "#e06c75", "#98c379", "#d19a66", "#61afef", "#c678dd", "#56b6c2", "#ffffff"]),
        ..P
    }),
    // Solarized Light, by Ethan Schoonover (https://github.com/altercation/solarized), MIT. Its
    // body text (base00) reads under 4.5:1 on base3, so text is base02 and muted text base01.
    ("Solarized Light", Palette {
        bg: "#fdf6e3", sidebar: Some("#eee8d5"), surface: "#fffbee", surface_active: Some("#e6dfc8"),
        text: "#073642", text_muted: Some("#586e75"), accent: "#268bd2", danger: Some("#dc322f"),
        success: Some("#859900"), border: Some("#d9d2bd"), status_bar: Some("#eee8d5"),
        terminal_bg: Some("#fdf6e3"), editor_bg: Some("#fdf6e3"), selection: Some("#eee8d5"),
        unsaved: Some("#b58900"),
        syntax: ["#859900", "#2aa198", "#93a1a1", "#d33682", "#b58900", "#268bd2", "#586e75", "#859900", "#657b83", "#dc322f"],
        ansi: Some(["#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5",
                    "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4", "#93a1a1", "#fdf6e3"]),
        ..P
    }),
];
