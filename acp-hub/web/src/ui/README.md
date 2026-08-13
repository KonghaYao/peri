# acp-hub Solid UI primitives

`src/ui/index.ts` is the only public import surface for feature code. A primitive owns reusable behavior, accessibility, DOM prop filtering and visual-state classes; feature components own domain language and server state.

## Contracts

| Primitive | Owns |
| --- | --- |
| `Button`, `IconButton` | closed variants/sizes, busy/disabled coupling, safe `type=button` default, accessible icon labels, DOM prop filtering |
| `Icon` | finite 20×20 outline canvas, inherited color, consistent stroke/caps, decorative accessibility semantics and closed visual sizes |
| `Badge` | closed neutral/success/warning/error visual tones without owning domain status inference |
| `TextField`, `SelectField`, `Textarea` | label/description/error wiring, invalid semantics and native prop forwarding; `Textarea` also owns bounded auto-growth |
| `Dialog` | body portal, optional visible title/close chrome, stack-aware application inertness, focus entry/trap/return, topmost-only Escape dismissal, explicit non-dismissible mutation state |
| `Drawer` | persistent-wide/modal-compact navigation semantics, background inertness, stack-aware Escape, focus entry/trap/return and scrim dismissal |
| `Popover` | stack-aware non-modal edit surface, explicit non-dismissible mutation state, outside/Escape dismissal, focus entry/return |
| `Menu`, `MenuItem` | stack-aware menu roles, typed default/danger items, arrow/Home/End navigation, outside/Escape/Tab dismissal |
| `Status` | semantic tone, status role and optional polite live updates |
| `Spinner` | a single loading indicator and accessible label contract |
| `CopyButton` | clipboard success/failure feedback |
| `EmptyState` | restrained empty-state structure and action slot |
| `ToastViewport` | polite short-lived notification region, responsive placement and entry motion |
| `Tooltip` | delayed pointer/immediate keyboard supplemental help, disabled-control hover support and Escape dismissal |
| `breakpoints.ts` | the behavior/CSS compact and phone viewport contract |
| `keyboard.ts` | platform-aware display labels for shared primary-modifier shortcuts |
| `primitives.css` | the public visual entry point; imports tokens and owns every primitive's standalone base/state styles |
| `tokens.css` | the sole reusable design-token source for colors, type, elevation and layout; imported by `primitives.css` |

## Rules

1. Feature code imports from `../../ui`, never an implementation file.
2. Component-only props must be consumed with `splitProps`; they must not appear in rendered DOM.
3. Do not add a primitive for a single screen. Promote a pattern after it recurs or when behavior (focus, keyboard, busy state, ARIA) must be correct everywhere.
4. Apps import `primitives.css`, which imports `tokens.css`. Reusable color, typography, elevation and component-state styles never depend on `styles.css`; that file owns only panel/feature layout and contextual overrides. Feature components do not introduce near-duplicate hex values.
5. A primitive test renders the real Solid component in jsdom and verifies keyboard/focus/ARIA/prop behavior. Pure selector tests are supplementary, not substitutes.
6. Destructive domain operations remain confirmation flows in features; the primitive supplies mechanics, not business permission.
7. Responsive JavaScript imports viewport contracts from `breakpoints.ts`; do not duplicate viewport literals in features. Compact (`≤959px`) makes navigation modal, medium (`960–1199px`) keeps navigation structural but tightens density, and wide (`≥1200px`) restores the full reading rhythm. CSS mirrors both shared thresholds and source contracts guard them.
8. `Button` never submits implicitly. A feature that owns form submission must state `type="submit"`; every other button remains inert to its surrounding form.
9. Essential actions cannot be hover-only. Coarse pointers expose sidebar actions at full opacity and preserve a 44px target; selected rows expose their action on every pointer type.
10. Feature components provide icon geometry through `Icon`; they do not render bare SVG canvases or repeat paint and sizing rules. Flex layout belongs to the wrapper actually participating in layout—for example, a tooltip-wrapped Composer action aligns through `.ui-tooltip-anchor`, not a hidden child margin.

Run `bun run test` and `bun run build` before changing the public surface.
