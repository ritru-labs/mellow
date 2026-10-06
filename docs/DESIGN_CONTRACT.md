# Visual design contract

Claude's prototype is the visual reference. The native TUI should preserve its *intent*, not blindly reproduce browser pixels.

## Core palette

| Token | Hex | Role |
|---|---|---|
| canvas | `#0A0F14` | primary background |
| surface | `#0F161E` | subtle active rows/panels |
| elevated | `#17232E` | overlays |
| elevated-2 | `#223242` | stronger overlay separation |
| text | `#EAF2F7` | primary text |
| muted | `#93A2B2` | secondary text |
| faint | `#718396` | low-emphasis chrome |
| mint | `#68F0C0` | focus/action accent |
| warning | `#F4C76C` | caution |
| error | `#FF7E94` | destructive/error |

## Terminal tiers

- **TrueColor:** full palette.
- **256-color:** map to nearest safe ANSI-256 colors.
- **Baseline:** readable 16-color/attribute fallback; never encode meaning by color alone.

## Size targets

- Compact: 80×24
- Comfortable: 120×34
- Expanded: 160×45

No core action may disappear at 80×24. Secondary labels can collapse before editing space does.

## Rules

- Prefer whitespace and hierarchy to boxes around everything.
- Use mint sparingly for focus, active actions and positive confirmation.
- Avoid permanent AI sidebars.
- Every overlay must be dismissible with Escape.
- Text labels accompany semantic color where safety matters.
- Unicode symbols must have ASCII fallbacks.
- No mandatory patched/Nerd Font glyphs.
