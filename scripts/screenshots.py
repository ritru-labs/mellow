# /// script
# requires-python = ">=3.9"
# dependencies = ["pyte>=0.8", "pillow>=10"]
# ///
"""Regenerate the README and docs screenshots from the real mellow binary.

    uv run scripts/screenshots.py [path/to/mellow]

Each scene runs mellow in a pseudo-terminal of a fixed size on a small demo
project, presses keys like a user, then draws the terminal screen into a PNG
with a window frame. Nothing is mocked: what you see is what mellow drew.
Set MELLOW_SCREENSHOT_FONT to a monospace .ttf with box-drawing glyphs.
"""

import fcntl
import os
import pty
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

import pyte
from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "images"
BIN = sys.argv[1] if len(sys.argv) > 1 else shutil.which("mellow") or str(ROOT / "target/release/mellow")

FONT_CANDIDATES = [
    os.environ.get("MELLOW_SCREENSHOT_FONT", ""),
    str(Path.home() / "Library/Fonts/MesloLGMDZNerdFontMono-Regular.ttf"),
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
]
FONT_PATH = next(p for p in FONT_CANDIDATES if p and Path(p).exists())
BOLD_PATH = FONT_PATH.replace("-Regular", "-Bold") if Path(FONT_PATH.replace("-Regular", "-Bold")).exists() else FONT_PATH

ANSI = {
    "black": "#1d1f21", "red": "#e06c75", "green": "#98c379", "brown": "#e5c07b",
    "yellow": "#e5c07b", "blue": "#61afef", "magenta": "#c678dd", "cyan": "#56b6c2",
    "white": "#c8ccd4", "brightblack": "#5c6370", "brightred": "#ef8891",
    "brightgreen": "#b5e890", "brightyellow": "#f0d197", "brightblue": "#8cc6f5",
    "brightmagenta": "#d7a1e7", "brightcyan": "#7fd1db", "brightwhite": "#ffffff",
}

# ---------------------------------------------------------------- demo project

ROUTE_RS = '''//! Turns raw GPS points into a trail with distance and climb.

use crate::geo::{haversine_km, Point};

/// A recorded trail, ready to show on the map.
#[derive(Debug, Clone)]
pub struct Trail {
    pub name: String,
    pub points: Vec<Point>,
}

impl Trail {
    pub fn new(name: &str, points: Vec<Point>) -> Self {
        Self { name: name.to_owned(), points }
    }

    /// Total length in kilometres.
    pub fn distance_km(&self) -> f64 {
        self.points
            .windows(2)
            .map(|pair| haversine_km(&pair[0], &pair[1]))
            .sum()
    }

    /// Metres climbed, ignoring the way down.
    pub fn climb_m(&self) -> f64 {
        self.points
            .windows(2)
            .map(|pair| (pair[1].elevation - pair[0].elevation).max(0.0))
            .sum()
    }
}
'''

FORECAST_PY = '''"""Fetch a trail forecast and decide whether today is a good day."""

from dataclasses import dataclass


@dataclass
class Forecast:
    temperature_c: float
    wind_kmh: float
    rain_mm: float


def good_day(forecast: Forecast) -> bool:
    """Dry, calm and not too hot: time to hike."""
    if forecast.rain_mm > 1.0:
        return False
    if forecast.wind_kmh > 40:
        return False
    return 5 <= forecast.temperature_c <= 28


def summary(forecast: Forecast) -> str:
    verdict = "go" if good_day(forecast) else "stay home"
    return f"{forecast.temperature_c:.0f}°C, wind {forecast.wind_kmh:.0f} km/h: {verdict}"
'''

FILES = {
    "Cargo.toml": '[package]\nname = "trailmap"\nversion = "0.4.0"\nedition = "2024"\n',
    "README.md": "# trailmap\n\nPlan hikes from GPS tracks and the weather.\n",
    "src/main.rs": 'mod geo;\nmod route;\n\nfn main() {\n    println!("trailmap");\n}\n',
    "src/geo.rs": "pub struct Point {\n    pub lat: f64,\n    pub lon: f64,\n    pub elevation: f64,\n}\n",
    "src/route.rs": ROUTE_RS,
    "forecast/forecast.py": FORECAST_PY,
    "config.yaml": "trails:\n  - name: Ridge loop\n    max_wind_kmh: 40\n",
    "scripts/deploy.sh": "#!/bin/sh\nset -eu\ncargo build --release\n",
}


def make_project() -> Path:
    project = Path(tempfile.mkdtemp()) / "trailmap"
    for name, text in FILES.items():
        path = project / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    git = lambda *args: subprocess.run(["git", "-C", str(project), *args], check=True, capture_output=True)
    git("init", "-q", "-b", "main")
    git("config", "user.name", "Ada Lovelace")
    git("config", "user.email", "ada@example.com")
    git("add", ".")
    git("commit", "-q", "-m", "Trail distance from GPS points")
    git("commit", "-q", "--allow-empty", "-m", "Read config.yaml")
    git("commit", "-q", "--allow-empty", "-m", "Add the forecast service")
    # Uncommitted work, so the Git gutter and status have something to show.
    route = project / "src/route.rs"
    route.write_text(route.read_text().replace(
        "    /// Metres climbed, ignoring the way down.",
        "    /// Average speed for a walk that took `hours`.\n"
        "    pub fn pace_kmh(&self, hours: f64) -> f64 {\n"
        "        self.distance_km() / hours\n"
        "    }\n\n"
        "    /// Metres climbed, ignoring the way down.",
    ))
    (project / "forecast/forecast.py").write_text(FORECAST_PY.replace("> 40", "> 35"))
    return project


# ---------------------------------------------------------------- running

def run_scene(project: Path, args, cols, rows, settings, steps):
    home = Path(tempfile.mkdtemp())
    (home / "config/mellow").mkdir(parents=True)
    (home / "state/mellow").mkdir(parents=True)
    (home / "config/mellow/settings.conf").write_text(settings)
    (home / "state/mellow/onboarding-v1.seen").write_text("")
    (home / ".bashrc").write_text("PS1='\\[\\e[32m\\]~/trailmap\\[\\e[0m\\] $ '\n")

    screen = pyte.Screen(cols, rows)
    stream = pyte.ByteStream(screen)
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(project)
        env = {
            "HOME": str(home), "PATH": os.environ["PATH"], "TERM": "xterm-256color",
            "COLORTERM": "truecolor", "LANG": "en_US.UTF-8", "SHELL": "/bin/bash",
            "XDG_CONFIG_HOME": str(home / "config"), "XDG_STATE_HOME": str(home / "state"),
            "BASH_SILENCE_DEPRECATION_WARNING": "1",
        }
        os.execve(BIN, [BIN, *args], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def pump(seconds):
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([fd], [], [], 0.03)
            if not ready:
                continue
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                return
            stream.feed(chunk)
            if b"\x1b[6n" in chunk:
                os.write(fd, f"\x1b[{screen.cursor.y + 1};{screen.cursor.x + 1}R".encode())
            if b"\x1b[c" in chunk:
                os.write(fd, b"\x1b[?1;2c")

    pump(2.5)
    for step in steps:
        if isinstance(step, (int, float)):
            pump(step)
        else:
            os.write(fd, step if isinstance(step, bytes) else step.encode())
            pump(0.35)
    pump(1.5)
    os.write(fd, b"\x11")  # Ctrl+Q
    pump(0.5)
    try:
        os.kill(pid, 9)
        os.waitpid(pid, 0)
    except OSError:
        pass
    return screen


# ---------------------------------------------------------------- drawing

def hex_color(value, fallback):
    if value in (None, "default"):
        return fallback
    if value in ANSI:
        return ANSI[value]
    if len(value) == 6:
        return "#" + value
    return fallback


def render(screen, title, scale=1.0):
    size = round(26 * scale)
    regular = ImageFont.truetype(FONT_PATH, size)
    bold = ImageFont.truetype(BOLD_PATH, size)
    cell_w = regular.getlength("M")
    ascent, descent = regular.getmetrics()
    cell_h = ascent + descent + round(4 * scale)

    counts = {}
    for y in range(screen.lines):
        for x in range(screen.columns):
            bg = screen.buffer[y][x].bg
            if bg != "default":
                counts[bg] = counts.get(bg, 0) + 1
    base_bg = hex_color(max(counts, key=counts.get) if counts else None, "#1a1b26")

    width = round(cell_w * screen.columns)
    height = cell_h * screen.lines
    body = Image.new("RGB", (width, height), base_bg)
    draw = ImageDraw.Draw(body)
    for y in range(screen.lines):
        row = screen.buffer[y]
        for x in range(screen.columns):
            ch = row[x]
            fg = hex_color(ch.fg, "#c8ccd4")
            bg = hex_color(ch.bg, base_bg)
            if ch.reverse:
                fg, bg = bg, fg
            left = round(x * cell_w)
            right = round((x + 1) * cell_w)
            if bg != base_bg:
                draw.rectangle([left, y * cell_h, right, (y + 1) * cell_h], fill=bg)
            if ch.data.strip():
                font = bold if ch.bold else regular
                draw.text((left, y * cell_h + round(2 * scale)), ch.data, font=font, fill=fg)
    if not screen.cursor.hidden:
        cx = round(screen.cursor.x * cell_w)
        draw.rectangle([cx, screen.cursor.y * cell_h + 2, cx + max(2, round(2 * scale)), (screen.cursor.y + 1) * cell_h - 2], fill="#9ece6a")

    # Window frame with a title bar, rounded corners and a soft shadow.
    bar = round(44 * scale)
    radius = round(14 * scale)
    window = Image.new("RGBA", (width, height + bar), (0, 0, 0, 0))
    mask = Image.new("L", window.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, window.width - 1, window.height - 1], radius, fill=255)
    chrome = Image.new("RGB", window.size, "#2b2d3a")
    chrome.paste(body, (0, bar))
    cd = ImageDraw.Draw(chrome)
    for i, colour in enumerate(["#ff5f57", "#febc2e", "#28c840"]):
        cx = round((22 + i * 22) * scale)
        r = round(7 * scale)
        cd.ellipse([cx - r, bar // 2 - r, cx + r, bar // 2 + r], fill=colour)
    label = ImageFont.truetype(FONT_PATH, round(20 * scale))
    tw = cd.textlength(title, font=label)
    cd.text(((window.width - tw) / 2, bar / 2 - round(12 * scale)), title, font=label, fill="#a9adc1")
    window.paste(chrome, (0, 0), mask)

    margin = round(48 * scale)
    canvas = Image.new("RGBA", (window.width + 2 * margin, window.height + 2 * margin), (0, 0, 0, 0))
    shadow = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle(
        [margin, margin + round(10 * scale), margin + window.width, margin + window.height + round(10 * scale)],
        radius, fill=(0, 0, 0, 110))
    shadow = shadow.filter(ImageFilter.GaussianBlur(round(18 * scale)))
    canvas = Image.alpha_composite(canvas, shadow)
    canvas.paste(window, (margin, margin), window)
    return canvas


def label_grid(images, labels, columns=2, scale=1.0):
    font = ImageFont.truetype(BOLD_PATH, round(30 * scale))
    w = max(i.width for i in images)
    h = max(i.height for i in images)
    label_h = round(44 * scale)
    rows = (len(images) + columns - 1) // columns
    grid = Image.new("RGBA", (w * columns, (h + label_h) * rows), (0, 0, 0, 0))
    d = ImageDraw.Draw(grid)
    for index, (image, text) in enumerate(zip(images, labels)):
        gx, gy = (index % columns) * w, (index // columns) * (h + label_h)
        tw = d.textlength(text, font=font)
        d.text((gx + (w - tw) / 2, gy + round(6 * scale)), text, font=font, fill="#8b8fa3")
        grid.paste(image, (gx, gy + label_h), image)
    return grid


# ---------------------------------------------------------------- scenes

def main():
    OUT.mkdir(parents=True, exist_ok=True)
    project = make_project()
    print(f"mellow: {subprocess.run([BIN, '--version'], capture_output=True, text=True).stdout.strip()} | font: {Path(FONT_PATH).name}")

    # 1. The hero: files, Git changes, the editor and the built-in terminal.
    hero = run_scene(
        project, ["src/route.rs"], 120, 34,
        "theme = dark\nexplorer_visible = true\n",
        [b"\x14", 1.5, "git log --oneline -3\r", 1.0, b"\x14", 0.5,
         b"\x07", "28\r", 0.5, b"\x1b[C" * 8],
    )
    render(hero, "mellow — trailmap").save(OUT / "hero.png")

    # 2. The command palette: every command one search away.
    palette = run_scene(
        project, ["forecast/forecast.py"], 110, 30,
        "theme = tokyo-night\nexplorer_visible = true\n",
        [b"\x10", 0.6, "git", 0.6],
    )
    render(palette, "mellow — command palette").save(OUT / "palette.png")

    # 3. Settings, with format on save switched on.
    settings = run_scene(
        project, ["src/route.rs"], 110, 30,
        "theme = catppuccin-mocha\nexplorer_visible = true\nformat_on_save = true\n",
        [b"\x10", 0.5, "open settings", 0.5, b"\r", 0.6] + [b"\x1b[B"] * 7,
    )
    render(settings, "mellow — settings").save(OUT / "settings.png")

    # 4. Four themes on the same Python file.
    themes = [("dark", "Dark"), ("light", "Light"), ("tokyo-night", "Tokyo Night"), ("gruvbox-dark", "Gruvbox Dark")]
    shots = []
    for config, _ in themes:
        screen = run_scene(
            project, ["forecast/forecast.py"], 92, 26,
            f"theme = {config}\nexplorer_visible = true\n",
            [b"\x1b[B"] * 13 + [0.4],
        )
        shots.append(render(screen, f"theme = {config}", scale=0.8))
    label_grid(shots, [name for _, name in themes], scale=0.8).save(OUT / "themes.png")

    for name in ("hero.png", "palette.png", "settings.png", "themes.png"):
        image = Image.open(OUT / name)
        print(f"wrote docs/images/{name}  {image.width}x{image.height}")


if __name__ == "__main__":
    main()
