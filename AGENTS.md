# AGENTS.md

## Working agreement

`sgraffito` is a native Wayland drawing app, not a web app. Excalidraw is the reference for
**interaction and geometry**, not a source tree to port mechanically. Before implementing a new
user-facing feature, or changing an existing interaction, deconstruct the corresponding Excalidraw
feature and use that deconstruction to drive the design.

This applies to tools, gestures, selection, transforms, rendering, clipboard/paste, labels, history,
shortcuts, and persistence. It does not apply to features that are inherently Wayland- or
compositor-specific, such as layer surfaces, backdrops, output scaling, and the control socket.

## Mandatory Excalidraw reference pass

For every new feature:

1. **Observe the real behavior** at <https://excalidraw.com>: pointer-down vs. pointer-up, modifier
   keys, selection state after the action, undo granularity, and what stays active.
2. **Read the actual source** rather than relying on memory, blog posts, or the UI alone. Prefer a
   shallow checkout of <https://github.com/excalidraw/excalidraw> at `/tmp/excalidraw-src` when a
   local one is not already available. Do not add Excalidraw or its packages as dependencies.
3. **Write a short deconstruction** before editing `src/`. Put it in the task plan/design notes and
   carry the conclusion into the commit message when it changes behavior.
4. **Map it to sgraffito** explicitly: what is reused conceptually, what is impossible in a native
   Wayland app, and what is deliberately different.
5. **Implement, then verify** the actual surface. A unit test alone is not enough for a new
   interaction: exercise it headlessly with `examples/vinject.rs` when the change is pointer- or
   keyboard-driven.

If Excalidraw has no equivalent feature, say so explicitly and use the nearest analogous interaction
or design from first principles. Do not invent an Excalidraw precedent.

## Deconstruction template

Use these headings; omit only sections that genuinely do not apply.

```text
Excalidraw reference: <name and source paths>
Observed behavior: <gestures, modifiers, state after the action, shortcuts>
Data model: <what is stored, derived, normalized, optional, or transient>
Geometry/algorithm: <coordinate space, curves, hit testing, snapping, bounds>
Rendering: <strokes vs. fills, z-order, caches, anti-aliasing>
Undo/selection/persistence: <what is one undo step, what is selected, what is saved>
sgraffito mapping: <existing types/APIs to reuse; schema impact>
Deliberate deviations: <why this cannot or should not match Excalidraw>
Verification: <tests and smoke scenario>
```

The deconstruction must distinguish **stored data** from **render-time computation**. For example,
Excalidraw stores linear vertices but recomputes rounded splines at render time; do not persist
derived curve points unless the existing schema requires it.

## Excalidraw source map

Start with these files; follow imports instead of guessing.

| Area | Excalidraw source |
| --- | --- |
| Freedraw, shapes, ideal geometry | `packages/element/src/shape.ts`, `bounds.ts`, `distance.ts` |
| Linear elements and point editing | `packages/element/src/linearElementEditor.ts`, `transformHandles.ts` |
| Bucket fill | `packages/element/src/bucketFill.ts`, `packages/excalidraw/components/App.bucketFill.ts` |
| Bindings and arrows | `packages/element/src/binding.ts`, `arrows/` |
| Selection and transforms | `packages/element/src/selection.ts`, `transform.ts`, `resizeElements.ts`, `dragElements.ts` |
| Text and labels | `packages/element/src/textElement.ts`, `textWrapping.ts`, `textMeasurements.ts`, `packages/excalidraw/components/App.text.ts` |
| Duplication and z-order | `packages/element/src/duplicate.ts`, `sortElements.ts` |
| Clipboard | `packages/excalidraw/clipboard.ts` |
| History | `packages/excalidraw/history.ts`, `packages/excalidraw/actions/actionHistory.tsx` |
| Canvas rendering | `packages/excalidraw/renderer/staticScene.ts`, `renderNewElementScene.ts`, `helpers.ts` |
| Shortcuts and tool definitions | `packages/excalidraw/components/Tools.tsx`, `packages/common/src/constants.ts` |

The online editor is the authority for feel; the source is the authority for mechanics.

## Porting rules

- **No new dependencies.** Reimplement the required math and behavior in the existing code. Do not
  pull in React, Rough.js, perfect-freehand, `points-on-curve`, or Excalidraw packages.
- **Preserve the annotation schema:** `VERSION = 6`, `READABLE_VERSION = 7`. New fields must be
  optional and backward-compatible; older readers must still open the file.
- **Keep raw input raw.** Store pointer samples and user-visible vertices; smooth, sample, and
  round at render time unless there is a proven reason to persist derived geometry.
- **One undo step per user action.** A gesture, a fill, or a style change is one entry; internal
  redraws and cache updates are not.
- **Do not copy a web interaction verbatim when Wayland differs.** Native pointer capture, keyboard
  focus, compositor clipboard, per-output scaling, and frame scheduling are app-specific. State the
  deviation in the deconstruction.
- **Match existing architecture.** Reuse `Transform`/`Affine`, `SketchCache`, the document raster,
  and the existing selection/undo APIs before adding parallel machinery.
- **Do not add an in-app element clipboard.** `Ctrl+C`/`Ctrl+X` do not copy elements; `Ctrl+D`
  duplicates directly, and `Ctrl+V` is reserved for compositor text.
- **Follow the current shortcut table** in `README.md` and `src/input.rs`. A new binding must update
  both, plus the shortcut consistency test in `src/render.rs` and `tests/keys.rs`.

## Verification bar

- Run `cargo fmt`.
- Run `cargo clippy --all-targets -- -D warnings`.
- Run the narrowest relevant tests, then `cargo test`.
- For UI behavior, run the headless sway smoke or a focused `vinject` sequence and inspect the
  written annotations or screenshot; report what was observed.
- The known baseline failure `tests/render.rs::the_preedit_renders_at_the_cursor` is unrelated
  unless the change touches preedit placement.

## Scope discipline

Do not expand a feature to "full Excalidraw parity" unless the task asks for it. Implement the
requested behavior end to end, state the remaining differences explicitly, and leave the codebase
with one convention rather than two.
