use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePreset {
    Dark,
    Light,
    HighContrast,
    TokyoNight,
    CatppuccinMocha,
    GruvboxDark,
}

impl ThemePreset {
    /// Cycle order for "Cycle Theme" and the Settings row.
    pub const ALL: [Self; 6] = [
        Self::Dark,
        Self::Light,
        Self::HighContrast,
        Self::TokyoNight,
        Self::CatppuccinMocha,
        Self::GruvboxDark,
    ];

    pub const fn next(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::HighContrast,
            Self::HighContrast => Self::TokyoNight,
            Self::TokyoNight => Self::CatppuccinMocha,
            Self::CatppuccinMocha => Self::GruvboxDark,
            Self::GruvboxDark => Self::Dark,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::HighContrast => "High Contrast",
            Self::TokyoNight => "Tokyo Night",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::GruvboxDark => "Gruvbox Dark",
        }
    }

    /// The value written as `theme = ...` in settings.conf.
    pub const fn config_name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::HighContrast => "high-contrast",
            Self::TokyoNight => "tokyo-night",
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::GruvboxDark => "gruvbox-dark",
        }
    }

    /// Parses a settings.conf theme name, accepting `_` for `-` and the
    /// short family names people usually type.
    pub fn from_config_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase().replace('_', "-");
        Some(match name.as_str() {
            "dark" => Self::Dark,
            "light" => Self::Light,
            "high-contrast" => Self::HighContrast,
            "tokyo-night" | "tokyonight" => Self::TokyoNight,
            "catppuccin-mocha" | "catppuccin" => Self::CatppuccinMocha,
            "gruvbox-dark" | "gruvbox" => Self::GruvboxDark,
            _ => return None,
        })
    }

    /// Stable number stored in session files, which older Mellow versions
    /// read back (they show unknown numbers as Dark). Never renumber.
    pub const fn index(self) -> u8 {
        match self {
            Self::Dark => 0,
            Self::Light => 1,
            Self::HighContrast => 2,
            Self::TokyoNight => 3,
            Self::CatppuccinMocha => 4,
            Self::GruvboxDark => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorTier {
    TrueColor,
    Ansi256,
    Basic,
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub unicode_symbols: bool,
    pub canvas: Color,
    pub surface: Color,
    pub elevated: Color,
    pub elevated2: Color,
    pub cur_line: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub mint: Color,
    pub warning: Color,
    pub error: Color,
    pub keyword: Color,
    pub string: Color,
    pub number: Color,
    pub function: Color,
    pub constant: Color,
    pub comment: Color,
    pub punctuation: Color,
    pub selection: Color,
    /// Diff rows: added / removed backgrounds and the text drawn on them.
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
    pub diff_text: Color,
}

impl Theme {
    #[cfg(test)]
    pub fn detect() -> Self {
        Self::for_preset(ThemePreset::Dark)
    }

    pub fn for_preset(preset: ThemePreset) -> Self {
        let mut theme = match (detect_color_tier(), preset) {
            (ColorTier::TrueColor, ThemePreset::Dark) => Self::true_color(),
            (ColorTier::TrueColor, ThemePreset::Light) => Self::true_color_light(),
            (ColorTier::TrueColor, ThemePreset::HighContrast) => Self::true_color_high_contrast(),
            (ColorTier::Ansi256, ThemePreset::Dark) => Self::ansi256(),
            (ColorTier::Ansi256, ThemePreset::Light) => Self::ansi256_light(),
            (ColorTier::Ansi256, ThemePreset::HighContrast) => Self::ansi256_high_contrast(),
            (ColorTier::Basic, ThemePreset::Dark) => Self::basic(),
            (ColorTier::Basic, ThemePreset::Light) => Self::basic_light(),
            (ColorTier::Basic, ThemePreset::HighContrast) => Self::basic_high_contrast(),
            // The community themes are defined once in TrueColor; 256-colour
            // terminals get the nearest palette entries, and 16-colour ones
            // the basic dark palette, since all three are dark themes.
            (ColorTier::TrueColor, ThemePreset::TokyoNight) => Self::tokyo_night(),
            (ColorTier::TrueColor, ThemePreset::CatppuccinMocha) => Self::catppuccin_mocha(),
            (ColorTier::TrueColor, ThemePreset::GruvboxDark) => Self::gruvbox_dark(),
            (ColorTier::Ansi256, ThemePreset::TokyoNight) => Self::tokyo_night().quantized(),
            (ColorTier::Ansi256, ThemePreset::CatppuccinMocha) => {
                Self::catppuccin_mocha().quantized()
            }
            (ColorTier::Ansi256, ThemePreset::GruvboxDark) => Self::gruvbox_dark().quantized(),
            (
                ColorTier::Basic,
                ThemePreset::TokyoNight | ThemePreset::CatppuccinMocha | ThemePreset::GruvboxDark,
            ) => Self::basic(),
        };
        theme.unicode_symbols = supports_unicode_symbols();
        theme
    }

    fn true_color() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::Rgb(10, 15, 20),         // #0A0F14
            surface: Color::Rgb(15, 22, 30),        // #0F161E
            elevated: Color::Rgb(23, 35, 46),       // #17232E
            elevated2: Color::Rgb(34, 50, 66),      // #223242
            cur_line: Color::Rgb(14, 21, 28),       // #0E151C
            text: Color::Rgb(234, 242, 247),        // #EAF2F7
            muted: Color::Rgb(147, 162, 178),       // #93A2B2
            faint: Color::Rgb(113, 131, 150),       // #718396
            mint: Color::Rgb(104, 240, 192),        // #68F0C0
            warning: Color::Rgb(244, 199, 108),     // #F4C76C
            error: Color::Rgb(255, 126, 148),       // #FF7E94
            keyword: Color::Rgb(143, 180, 255),     // #8FB4FF soft lavender-blue
            string: Color::Rgb(230, 196, 154),      // #E6C49A warm gold
            number: Color::Rgb(242, 163, 131),      // #F2A383 coral peach
            function: Color::Rgb(126, 217, 232),    // #7ED9E8 sky cyan
            constant: Color::Rgb(201, 182, 255),    // #C9B6FF violet
            comment: Color::Rgb(122, 140, 158),     // #7A8C9E slate
            punctuation: Color::Rgb(147, 162, 178), // #93A2B2
            selection: Color::Rgb(42, 88, 98),      // #2A5862 teal selection, clearly visible
            diff_add_bg: Color::Rgb(18, 48, 42),
            diff_del_bg: Color::Rgb(58, 26, 36),
            diff_text: Color::Rgb(234, 242, 247),
        }
    }

    fn true_color_light() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::Rgb(248, 250, 252),
            surface: Color::Rgb(240, 244, 248),
            elevated: Color::Rgb(229, 235, 241),
            elevated2: Color::Rgb(214, 224, 234),
            cur_line: Color::Rgb(235, 243, 248),
            text: Color::Rgb(24, 32, 40),
            muted: Color::Rgb(75, 92, 108),
            faint: Color::Rgb(104, 119, 132),
            mint: Color::Rgb(0, 125, 96),
            warning: Color::Rgb(145, 91, 0),
            error: Color::Rgb(176, 37, 65),
            keyword: Color::Rgb(45, 83, 170),
            string: Color::Rgb(130, 84, 25),
            number: Color::Rgb(166, 75, 42),
            function: Color::Rgb(0, 110, 135),
            constant: Color::Rgb(100, 72, 170),
            comment: Color::Rgb(92, 108, 121),
            punctuation: Color::Rgb(75, 92, 108),
            selection: Color::Rgb(185, 225, 215),
            diff_add_bg: Color::Rgb(210, 240, 222),
            diff_del_bg: Color::Rgb(250, 216, 220),
            diff_text: Color::Rgb(24, 32, 40),
        }
    }

    fn true_color_high_contrast() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::Rgb(0, 0, 0),
            surface: Color::Rgb(0, 0, 0),
            elevated: Color::Rgb(12, 12, 12),
            elevated2: Color::Rgb(32, 32, 32),
            cur_line: Color::Rgb(18, 18, 18),
            text: Color::Rgb(255, 255, 255),
            muted: Color::Rgb(214, 214, 214),
            faint: Color::Rgb(174, 174, 174),
            mint: Color::Rgb(0, 255, 190),
            warning: Color::Rgb(255, 220, 80),
            error: Color::Rgb(255, 90, 115),
            keyword: Color::Rgb(115, 190, 255),
            string: Color::Rgb(255, 220, 130),
            number: Color::Rgb(255, 160, 120),
            function: Color::Rgb(100, 235, 255),
            constant: Color::Rgb(220, 185, 255),
            comment: Color::Rgb(180, 195, 205),
            punctuation: Color::Rgb(235, 235, 235),
            selection: Color::Rgb(0, 90, 75),
            diff_add_bg: Color::Rgb(0, 72, 40),
            diff_del_bg: Color::Rgb(100, 0, 24),
            diff_text: Color::Rgb(255, 255, 255),
        }
    }

    fn ansi256() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::Indexed(233),
            surface: Color::Indexed(234),
            elevated: Color::Indexed(235),
            elevated2: Color::Indexed(237),
            cur_line: Color::Indexed(234),
            text: Color::Indexed(255),
            muted: Color::Indexed(249),
            faint: Color::Indexed(244),
            mint: Color::Indexed(121),
            warning: Color::Indexed(221),
            error: Color::Indexed(211),
            keyword: Color::Indexed(111),
            string: Color::Indexed(180),
            number: Color::Indexed(216),
            function: Color::Indexed(116),
            constant: Color::Indexed(147),
            comment: Color::Indexed(244),
            punctuation: Color::Indexed(248),
            selection: Color::Indexed(30),
            diff_add_bg: Color::Indexed(22),
            diff_del_bg: Color::Indexed(52),
            diff_text: Color::Indexed(255),
        }
    }

    fn ansi256_light() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::Indexed(231),
            surface: Color::Indexed(255),
            elevated: Color::Indexed(254),
            elevated2: Color::Indexed(252),
            cur_line: Color::Indexed(195),
            text: Color::Indexed(233),
            muted: Color::Indexed(240),
            faint: Color::Indexed(244),
            mint: Color::Indexed(29),
            warning: Color::Indexed(130),
            error: Color::Indexed(160),
            keyword: Color::Indexed(25),
            string: Color::Indexed(94),
            number: Color::Indexed(166),
            function: Color::Indexed(30),
            constant: Color::Indexed(91),
            comment: Color::Indexed(244),
            punctuation: Color::Indexed(240),
            selection: Color::Indexed(158),
            diff_add_bg: Color::Indexed(194),
            diff_del_bg: Color::Indexed(224),
            diff_text: Color::Indexed(233),
        }
    }

    fn ansi256_high_contrast() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::Indexed(16),
            surface: Color::Indexed(16),
            elevated: Color::Indexed(232),
            elevated2: Color::Indexed(236),
            cur_line: Color::Indexed(233),
            text: Color::Indexed(231),
            muted: Color::Indexed(252),
            faint: Color::Indexed(248),
            mint: Color::Indexed(49),
            warning: Color::Indexed(226),
            error: Color::Indexed(210),
            keyword: Color::Indexed(117),
            string: Color::Indexed(229),
            number: Color::Indexed(216),
            function: Color::Indexed(123),
            constant: Color::Indexed(183),
            comment: Color::Indexed(250),
            punctuation: Color::Indexed(255),
            selection: Color::Indexed(30),
            diff_add_bg: Color::Indexed(22),
            diff_del_bg: Color::Indexed(52),
            diff_text: Color::Indexed(231),
        }
    }

    fn basic() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::Black,
            surface: Color::Black,
            elevated: Color::Black,
            elevated2: Color::DarkGray,
            cur_line: Color::Black,
            text: Color::White,
            muted: Color::Gray,
            faint: Color::DarkGray,
            mint: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            keyword: Color::Blue,
            string: Color::Yellow,
            number: Color::Magenta,
            function: Color::Cyan,
            constant: Color::Magenta,
            comment: Color::DarkGray,
            punctuation: Color::White,
            selection: Color::Blue,
            diff_add_bg: Color::Green,
            diff_del_bg: Color::Red,
            diff_text: Color::Black,
        }
    }

    fn basic_light() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::White,
            surface: Color::White,
            elevated: Color::Gray,
            elevated2: Color::DarkGray,
            cur_line: Color::Gray,
            text: Color::Black,
            muted: Color::DarkGray,
            faint: Color::DarkGray,
            mint: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            keyword: Color::Blue,
            string: Color::Yellow,
            number: Color::Magenta,
            function: Color::Cyan,
            constant: Color::Magenta,
            comment: Color::DarkGray,
            punctuation: Color::Black,
            selection: Color::Cyan,
            diff_add_bg: Color::Green,
            diff_del_bg: Color::Red,
            diff_text: Color::Black,
        }
    }

    fn basic_high_contrast() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::Black,
            surface: Color::Black,
            elevated: Color::Black,
            elevated2: Color::DarkGray,
            cur_line: Color::DarkGray,
            text: Color::White,
            muted: Color::White,
            faint: Color::Gray,
            mint: Color::Cyan,
            warning: Color::Yellow,
            error: Color::Red,
            keyword: Color::Cyan,
            string: Color::Yellow,
            number: Color::Magenta,
            function: Color::Cyan,
            constant: Color::Magenta,
            comment: Color::Gray,
            punctuation: Color::White,
            selection: Color::Blue,
            diff_add_bg: Color::Green,
            diff_del_bg: Color::Red,
            diff_text: Color::Black,
        }
    }

    /// Tokyo Night ("night"), after folke/tokyonight.nvim (MIT). Comments
    /// and line numbers use the palette's lighter `dark5` so they stay
    /// readable (4.6:1) instead of the original's 2.6:1.
    fn tokyo_night() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::Rgb(26, 27, 38),         // #1A1B26 bg
            surface: Color::Rgb(22, 22, 30),        // #16161E bg_dark
            elevated: Color::Rgb(31, 35, 53),       // #1F2335 bg_float
            elevated2: Color::Rgb(47, 53, 73),      // #2F3549
            cur_line: Color::Rgb(31, 33, 48),       // #1F2130
            text: Color::Rgb(192, 202, 245),        // #C0CAF5 fg
            muted: Color::Rgb(169, 177, 214),       // #A9B1D6 fg_dark
            faint: Color::Rgb(124, 131, 171),       // #7C83AB
            mint: Color::Rgb(115, 218, 202),        // #73DACA green1
            warning: Color::Rgb(224, 175, 104),     // #E0AF68 yellow
            error: Color::Rgb(247, 118, 142),       // #F7768E red
            keyword: Color::Rgb(187, 154, 247),     // #BB9AF7 magenta
            string: Color::Rgb(158, 206, 106),      // #9ECE6A green
            number: Color::Rgb(255, 158, 100),      // #FF9E64 orange
            function: Color::Rgb(122, 162, 247),    // #7AA2F7 blue
            constant: Color::Rgb(42, 195, 222),     // #2AC3DE blue1 (types)
            comment: Color::Rgb(124, 131, 171),     // #7C83AB
            punctuation: Color::Rgb(137, 221, 255), // #89DDFF blue5
            selection: Color::Rgb(46, 60, 100),     // #2E3C64
            diff_add_bg: Color::Rgb(32, 48, 59),    // #20303B
            diff_del_bg: Color::Rgb(55, 34, 44),    // #37222C
            diff_text: Color::Rgb(192, 202, 245),
        }
    }

    /// Catppuccin Mocha, after catppuccin/catppuccin (MIT).
    fn catppuccin_mocha() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::Rgb(30, 30, 46),         // #1E1E2E base
            surface: Color::Rgb(24, 24, 37),        // #181825 mantle
            elevated: Color::Rgb(36, 39, 58),       // #24273A
            elevated2: Color::Rgb(54, 58, 79),      // #363A4F
            cur_line: Color::Rgb(35, 35, 52),       // #232334
            text: Color::Rgb(205, 214, 244),        // #CDD6F4 text
            muted: Color::Rgb(186, 194, 222),       // #BAC2DE subtext1
            faint: Color::Rgb(139, 143, 168),       // #8B8FA8
            mint: Color::Rgb(148, 226, 213),        // #94E2D5 teal
            warning: Color::Rgb(249, 226, 175),     // #F9E2AF yellow
            error: Color::Rgb(243, 139, 168),       // #F38BA8 red
            keyword: Color::Rgb(203, 166, 247),     // #CBA6F7 mauve
            string: Color::Rgb(166, 227, 161),      // #A6E3A1 green
            number: Color::Rgb(250, 179, 135),      // #FAB387 peach
            function: Color::Rgb(137, 180, 250),    // #89B4FA blue
            constant: Color::Rgb(249, 226, 175),    // #F9E2AF yellow (types)
            comment: Color::Rgb(147, 153, 178),     // #9399B2 overlay2
            punctuation: Color::Rgb(147, 153, 178), // #9399B2 overlay2
            selection: Color::Rgb(62, 64, 88),      // #3E4058
            diff_add_bg: Color::Rgb(43, 58, 51),    // #2B3A33
            diff_del_bg: Color::Rgb(63, 42, 53),    // #3F2A35
            diff_text: Color::Rgb(205, 214, 244),
        }
    }

    /// Gruvbox dark (medium), after morhetz/gruvbox (MIT). Its bright red
    /// is lifted from #FB4934 to #FF6655 so keywords and errors pass 4.5:1.
    fn gruvbox_dark() -> Self {
        Self {
            unicode_symbols: true,
            canvas: Color::Rgb(40, 40, 40),         // #282828 bg0
            surface: Color::Rgb(29, 32, 33),        // #1D2021 bg0_h
            elevated: Color::Rgb(50, 48, 47),       // #32302F bg0_s
            elevated2: Color::Rgb(69, 64, 61),      // #45403D
            cur_line: Color::Rgb(46, 44, 43),       // #2E2C2B
            text: Color::Rgb(235, 219, 178),        // #EBDBB2 fg1
            muted: Color::Rgb(213, 196, 161),       // #D5C4A1 fg2
            faint: Color::Rgb(168, 153, 132),       // #A89984 fg4
            mint: Color::Rgb(142, 192, 124),        // #8EC07C aqua
            warning: Color::Rgb(250, 189, 47),      // #FABD2F yellow
            error: Color::Rgb(255, 102, 85),        // #FF6655 red (lifted)
            keyword: Color::Rgb(255, 102, 85),      // #FF6655 red (lifted)
            string: Color::Rgb(184, 187, 38),       // #B8BB26 green
            number: Color::Rgb(211, 134, 155),      // #D3869B purple
            function: Color::Rgb(142, 192, 124),    // #8EC07C aqua
            constant: Color::Rgb(250, 189, 47),     // #FABD2F yellow (types)
            comment: Color::Rgb(168, 153, 132),     // #A89984 fg4
            punctuation: Color::Rgb(189, 174, 147), // #BDAE93 fg3
            selection: Color::Rgb(80, 73, 69),      // #504945 bg2
            diff_add_bg: Color::Rgb(50, 54, 26),    // #32361A
            diff_del_bg: Color::Rgb(60, 31, 30),    // #3C1F1E
            diff_text: Color::Rgb(235, 219, 178),
        }
    }

    /// The same theme for a 256-colour terminal: every colour becomes its
    /// nearest xterm palette entry.
    fn quantized(self) -> Self {
        let q = nearest_ansi256;
        let mut theme = Self {
            unicode_symbols: self.unicode_symbols,
            canvas: q(self.canvas),
            surface: q(self.surface),
            elevated: q(self.elevated),
            elevated2: q(self.elevated2),
            cur_line: q(self.cur_line),
            text: q(self.text),
            muted: q(self.muted),
            faint: q(self.faint),
            mint: q(self.mint),
            warning: q(self.warning),
            error: q(self.error),
            keyword: q(self.keyword),
            string: q(self.string),
            number: q(self.number),
            function: q(self.function),
            constant: q(self.constant),
            comment: q(self.comment),
            punctuation: q(self.punctuation),
            selection: q(self.selection),
            diff_add_bg: q(self.diff_add_bg),
            diff_del_bg: q(self.diff_del_bg),
            diff_text: q(self.diff_text),
        };
        // Near-black backgrounds can land on the same grey; keep the
        // current line visible by lifting it one step up the grey ramp.
        if theme.cur_line == theme.canvas
            && let Color::Indexed(index @ 232..=254) = theme.canvas
        {
            theme.cur_line = Color::Indexed(index + 1);
        }
        theme
    }
}

/// Nearest xterm-256 entry (6×6×6 cube or 24-step grey ramp) for an RGB
/// colour; other colours pass through unchanged.
fn nearest_ansi256(color: Color) -> Color {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    let step = |value: u8| {
        (0..6)
            .min_by_key(|&i| (i32::from(LEVELS[i]) - i32::from(value)).abs())
            .unwrap_or(0)
    };
    let distance = |(x, y, z): (u8, u8, u8)| {
        [(x, r), (y, g), (z, b)]
            .iter()
            .map(|&(a, b)| (i32::from(a) - i32::from(b)).pow(2))
            .sum::<i32>()
    };

    let (ri, gi, bi) = (step(r), step(g), step(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;

    let average = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let grey_step = (0..24u16)
        .min_by_key(|&i| (i32::from(8 + 10 * i) - i32::from(average)).abs())
        .unwrap_or(0);
    let level = (8 + 10 * grey_step) as u8;

    if distance(cube) <= distance((level, level, level)) {
        Color::Indexed(cube_index as u8)
    } else {
        Color::Indexed(232 + grey_step as u8)
    }
}

fn supports_unicode_symbols() -> bool {
    locale_is_unicode(|name| std::env::var(name).ok())
}

/// The first of LC_ALL, LC_CTYPE and LANG that is set decides. Empty values
/// count as unset (as in POSIX), so an exported `LC_ALL=` no longer turned
/// every symbol into ASCII.
fn locale_is_unicode(lookup: impl Fn(&str) -> Option<String>) -> bool {
    for name in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Some(value) = lookup(name).filter(|value| !value.is_empty()) {
            let lower = value.to_ascii_lowercase();
            return lower.contains("utf-8") || lower.contains("utf8");
        }
    }
    true
}

pub fn detect_color_tier() -> ColorTier {
    let colorterm = std::env::var("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return ColorTier::TrueColor;
    }

    let term = std::env::var("TERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if term.contains("256color") {
        ColorTier::Ansi256
    } else {
        ColorTier::Basic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(color: Color) -> (f64, f64, f64) {
        let (r, g, b) = match color {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(index @ 16..=231) => {
                let level = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
                let n = index - 16;
                (level(n / 36), level((n / 6) % 6), level(n % 6))
            }
            Color::Indexed(index @ 232..=255) => {
                let v = 8 + (index - 232) * 10;
                (v, v, v)
            }
            other => panic!("no fixed RGB for {other:?}"),
        };
        (f64::from(r), f64::from(g), f64::from(b))
    }

    fn contrast(a: Color, b: Color) -> f64 {
        let luminance = |color| {
            let (r, g, b) = rgb(color);
            let channel = |c: f64| {
                let c = c / 255.0;
                if c <= 0.039_28 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
        };
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// Audit: diffs used fixed dark backgrounds, leaving Light-theme text at
    /// about 1.1:1. Every fixed-colour theme must keep diff text readable.
    #[test]
    fn diff_text_is_readable_on_diff_backgrounds_in_every_theme() {
        for (name, theme) in [
            ("dark", Theme::true_color()),
            ("light", Theme::true_color_light()),
            ("high contrast", Theme::true_color_high_contrast()),
            ("256 dark", Theme::ansi256()),
            ("256 light", Theme::ansi256_light()),
            ("256 high contrast", Theme::ansi256_high_contrast()),
            ("tokyo night", Theme::tokyo_night()),
            ("catppuccin mocha", Theme::catppuccin_mocha()),
            ("gruvbox dark", Theme::gruvbox_dark()),
            ("256 tokyo night", Theme::tokyo_night().quantized()),
            (
                "256 catppuccin mocha",
                Theme::catppuccin_mocha().quantized(),
            ),
            ("256 gruvbox dark", Theme::gruvbox_dark().quantized()),
        ] {
            for (kind, bg) in [("added", theme.diff_add_bg), ("removed", theme.diff_del_bg)] {
                let ratio = contrast(theme.diff_text, bg);
                assert!(ratio >= 4.5, "{name} {kind}: {ratio:.2}:1");
            }
        }
    }

    /// The community themes are checked against how the UI actually pairs
    /// each role: body text on every surface, syntax on the editor canvas
    /// and current line, hints on panels and dialogs.
    fn assert_readable(name: &str, theme: &Theme, syntax_minimum: f64) {
        let check = |fg: (&str, Color), bg: (&str, Color), minimum: f64| {
            let ratio = contrast(fg.1, bg.1);
            assert!(
                ratio >= minimum,
                "{name}: {} on {} is {ratio:.2}:1, needs {minimum}:1",
                fg.0,
                bg.0
            );
        };
        let canvas = ("canvas", theme.canvas);
        let cur_line = ("cur_line", theme.cur_line);
        let surface = ("surface", theme.surface);
        let elevated = ("elevated", theme.elevated);
        for bg in [canvas, cur_line, surface, elevated] {
            check(("text", theme.text), bg, 7.0);
        }
        for bg in [
            ("elevated2", theme.elevated2),
            ("selection", theme.selection),
        ] {
            check(("text", theme.text), bg, 4.5);
        }
        for bg in [canvas, surface, elevated] {
            check(("muted", theme.muted), bg, 4.5);
            check(("faint", theme.faint), bg, 3.0);
        }
        for fg in [
            ("keyword", theme.keyword),
            ("string", theme.string),
            ("number", theme.number),
            ("function", theme.function),
            ("constant", theme.constant),
        ] {
            check(fg, canvas, syntax_minimum);
            check(fg, cur_line, syntax_minimum);
        }
        for fg in [
            ("comment", theme.comment),
            ("punctuation", theme.punctuation),
        ] {
            check(fg, canvas, 4.0);
        }
        for fg in [
            ("mint", theme.mint),
            ("warning", theme.warning),
            ("error", theme.error),
        ] {
            check(fg, canvas, syntax_minimum);
            check(fg, elevated, syntax_minimum);
        }
    }

    #[test]
    fn community_themes_are_readable_in_truecolor_and_256_colours() {
        for (name, theme) in [
            ("tokyo night", Theme::tokyo_night()),
            ("catppuccin mocha", Theme::catppuccin_mocha()),
            ("gruvbox dark", Theme::gruvbox_dark()),
        ] {
            assert_readable(name, &theme, 4.5);
            // Snapping to the 256-colour palette costs a little contrast.
            let quantized = theme.quantized();
            assert_readable(&format!("256 {name}"), &quantized, 4.0);
            assert_ne!(
                quantized.cur_line, quantized.canvas,
                "256 {name}: the current line must stay visible"
            );
        }
    }

    #[test]
    fn empty_locale_variables_count_as_unset() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        assert!(locale_is_unicode(env(&[
            ("LC_ALL", ""),
            ("LANG", "en_US.UTF-8")
        ])));
        assert!(locale_is_unicode(env(&[])));
        assert!(!locale_is_unicode(env(&[
            ("LC_ALL", "C"),
            ("LANG", "en_US.UTF-8")
        ])));
        assert!(!locale_is_unicode(env(&[
            ("LC_CTYPE", ""),
            ("LANG", "POSIX")
        ])));
    }

    #[test]
    fn nearest_ansi256_hits_exact_palette_entries() {
        assert_eq!(nearest_ansi256(Color::Rgb(0, 0, 0)), Color::Indexed(16));
        assert_eq!(
            nearest_ansi256(Color::Rgb(255, 255, 255)),
            Color::Indexed(231)
        );
        assert_eq!(
            nearest_ansi256(Color::Rgb(95, 135, 175)),
            Color::Indexed(67)
        );
        assert_eq!(
            nearest_ansi256(Color::Rgb(128, 128, 128)),
            Color::Indexed(244)
        );
        assert_eq!(nearest_ansi256(Color::Rgb(38, 38, 38)), Color::Indexed(235));
        assert_eq!(nearest_ansi256(Color::Blue), Color::Blue);
    }

    #[test]
    fn themes_have_distinct_primary_text_and_canvas() {
        for preset in ThemePreset::ALL {
            let theme = Theme::for_preset(preset);
            assert_ne!(theme.canvas, theme.text, "{}", preset.label());
        }
    }

    #[test]
    fn theme_preset_cycle_visits_every_theme_once() {
        assert_eq!(ThemePreset::Dark.next(), ThemePreset::Light);
        let mut seen = vec![ThemePreset::Dark];
        let mut preset = ThemePreset::Dark.next();
        while preset != ThemePreset::Dark {
            seen.push(preset);
            preset = preset.next();
        }
        assert_eq!(seen, ThemePreset::ALL);
    }

    #[test]
    fn theme_names_and_numbers_round_trip() {
        for preset in ThemePreset::ALL {
            assert_eq!(
                ThemePreset::from_config_name(preset.config_name()),
                Some(preset)
            );
        }
        // Session files store these numbers; older releases know 0..=2.
        let numbers: Vec<u8> = ThemePreset::ALL
            .iter()
            .map(|preset| preset.index())
            .collect();
        assert_eq!(numbers, [0, 1, 2, 3, 4, 5]);
        assert_eq!(
            ThemePreset::from_config_name("High_Contrast"),
            Some(ThemePreset::HighContrast)
        );
        assert_eq!(
            ThemePreset::from_config_name("gruvbox"),
            Some(ThemePreset::GruvboxDark)
        );
        assert_eq!(ThemePreset::from_config_name("solarized"), None);
    }
}
