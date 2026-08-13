# 0004 — Embedded service visual consistency

Vox, Memoria, and Forma render inside the Conduit Console (Vox and Memoria as `<iframe>` via `App.tsx:1181`; Forma natively as `FormaPanel`). Today each surface uses its own palette, accent hue, radii, spacing, and typography, and Forma is effectively unthemed because its CSS references custom properties the shell never defines. The operator sees three products, not one.

This spec defines a single visual contract for embedded services and lists the concrete edits required to bring Vox, Memoria, and Forma into it.

## Contract — shell tokens are the source of truth

Every embedded surface MUST resolve to the following values (either by inheriting the shell's CSS custom properties or by inlining these exact values in an iframe):

| Token | Value | Use |
|---|---|---|
| `--app-bg` | `#0a0c11` | page background |
| `--surface` | `#141720` | cards / sections |
| `--surface-raised` | `#1e2229` | hover / raised elements, code chips |
| `--surface-sunken` | `#07080c` | inputs, iframe container background |
| `--border` | `#2f343d` | default 1 px borders |
| `--border-strong` | `#464c58` | input borders |
| `--accent` | `#12a394` | primary action, selected state |
| `--accent-hover` | `#48d7c4` | primary hover |
| `--accent-quiet` | `rgba(18,163,148,0.14)` | selected-row tint, focus halo |
| `--focus` | `#48d7c4` | focus outline |
| `--text` | `#d5d8dd` | body copy |
| `--strong` | `#f4f5f6` | headings, emphasized text |
| `--muted` | `#8a909c` | secondary copy, eyebrows |
| `--success` | `#10b981` | ok status |
| `--warning-text` | `#f59e0b` | warn status |
| `--danger` | `#ef4444` | destructive action, error status |

Typography: `Inter, ui-sans-serif, system-ui, …` at 14 px / 1.45. Monospace: `ui-monospace, "SF Mono", Menlo, monospace`.

Radii: 6 px for form controls, 8 px for cards/sections/buttons. **No** 10 px.

Spacing scale: 4 / 8 / 12 / 16 / 24 / 32 px.

Focus: `outline: 2px solid var(--focus); outline-offset: 1px` on all interactive elements.

Color scheme: dark only. Panels MUST NOT branch on `prefers-color-scheme: light`.

## Component grammar

- **Section title.** 12 px uppercase, weight 700, color `--muted`, `letter-spacing: 0`. This is the "eyebrow" pattern from `App.css`. Applies to every panel heading inside an embedded surface.
- **Card.** `background: var(--surface); border: 1px solid var(--border); border-radius: 8px; padding: 16px`.
- **Primary button.** `background: var(--accent); color: #0a0c11; border: 1px solid var(--accent)`. Hover → `--accent-hover`.
- **Secondary button.** `background: var(--surface-raised); color: var(--strong); border: 1px solid var(--border)`.
- **Danger button.** `background: transparent; color: var(--danger); border: 1px solid var(--danger)`. Hover fills with `--danger-bg`.
- **Button color encodes severity, never verb.** Add / Edit / Load / Search / Link are all primary. Delete / Unlink are danger. Everything else is secondary.
- **Input.** `min-height: 42px; background: var(--surface-sunken); border: 1px solid var(--border-strong); border-radius: 6px; padding: 0 12px; color: var(--strong)`.
- **Status dot.** 8 px circle. `--success` / `--warning-text` / `--danger` / `--muted`.
- **Notice / alert.** 8 px radius, 1 px border in the semantic color, background at 12 % alpha of the same hue.
- **Modal overlay.** `background: rgba(0,0,0,0.5)`; modal body is a card (see above).
- **Iconography.** `lucide` (self-hosted for iframes, npm for the shell). One library across all three surfaces.
- **Table.** Header row uses eyebrow style. Rows separated by 1 px `--border` bottom. Numeric columns `font-variant-numeric: tabular-nums`.

## Panel-specific requirements

### Forma (`frontend/src/forma/FormaPanel.css`)
1. Rename or alias every custom property to the shell token set:
   - `--bg-surface` → `--surface`
   - `--bg-subtle` → `--surface-raised`
   - `--bg-hover` → `--surface-raised`
   - `--border-subtle` → `--border`
   - `--border-default` → `--border-strong`
   - `--accent-primary` → `--accent`
   - `--accent-primary-dim` → `--accent-quiet`
   - `--text-primary` → `--strong`
   - `--text-secondary` → `--muted`
   - `--font-mono` → `ui-monospace, "SF Mono", Menlo, monospace` inlined
2. Card `border-radius` 8 px (some are already, `.rule-item` is `8px` — keep; the `.rule-editor` 8 px — keep).
3. Rule-editor modal overlay already correct; verify z-index does not fight the shell.

### Vox (`services/vox/static/index.html`)
1. Replace inline token values with the shell palette above. Delete the `@media (prefers-color-scheme: light)` branch and the light defaults.
2. Set `color-scheme: dark` on `:root`.
3. Retune accent from blue `#3d6cff` to `#12a394`; hover to `#48d7c4`.
4. Card radius 10 px → 8 px.
5. Font stack: prepend `Inter,`.
6. Header `<h1>` — keep single primary title. Section `<h2>` styled as eyebrow.
7. Notice borders/bg use `--danger` / `--success` / `--warning-text` tokens.
8. Focus ring uses `--focus`.

### Memoria (`services/memoria/static/index.html`)
1. Remove `cdn.tailwindcss.com` and `unpkg.com/lucide@latest` script tags. Replace with a hand-written `<style>` block matching Vox's approach, using the shell palette inlined.
2. Replace every `bg-purple-*`, `bg-blue-*`, `bg-green-*` action button with the primary button pattern above. Only `deleteLink`/destructive controls stay danger.
3. Icon color for status/decoration also standardizes: `--accent` for primary decoration, `--muted` for neutral, `--success/--warning-text/--danger` for status.
4. Flatten the internal top-nav (`Dashboard / Engrams / Search / Speakers / Link`) into a single scrolling page composed of cards, matching the Vox pattern. Multi-level chrome (shell rail + embedded tabs) is out.
5. Set `color-scheme: dark`; drop `bg-gray-*` colors.
6. Section titles adopt the eyebrow style.

## Non-goals

- Rewriting Vox or Memoria to React or bundling them into the frontend build.
- Changing information architecture beyond the Memoria flatten.
- Adding new features. This is a visual-consistency pass only.

## Verification

- Grep `services/{vox,memoria}/static/index.html` for `#3d6cff`, `purple`, `blue`, `gray-8`, `prefers-color-scheme: light`, `cdn.tailwindcss.com`, `unpkg.com`. Zero hits.
- Grep `frontend/src/forma/FormaPanel.css` for `--bg-`, `--text-primary`, `--text-secondary`, `--accent-primary`, `--border-subtle`, `--border-default`, `--font-mono`. Zero hits.
- Open Providers, Memory, and Pipelines panels in the Console. All three surfaces read as the same product: same background, same teal accent, same eyebrow section titles, same input/button chrome.

## Execution order

1. Forma token rename (P0 — panel is currently unthemed).
2. Vox palette + light-mode removal.
3. Memoria de-CDN + palette + button-severity fix.
4. Memoria nav flatten.
5. Cross-surface grammar sweep (eyebrow titles, focus rings, spacing).
