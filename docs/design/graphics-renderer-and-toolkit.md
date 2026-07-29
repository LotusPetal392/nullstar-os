# Native graphics renderer and UI toolkit direction

## Status

A native Rust vector-and-raster rendering engine, a Rust widget toolkit, SVG-first
icons and cursors, and a constrained CSS-derived theme language are **accepted
direction**. The crate names, public Rust API, retained-versus-immediate balance, GPU
backend, and exact style grammar remain **tentative design**.

The global surface and security boundary is defined by
[the graphics-stack design](graphics-stack.md). This document covers rendering and
widgets inside a client or trusted shell component.

## Layered architecture

The native UI stack should be separated into independently useful layers:

```text
application and widget state
        |
        v
native Rust widget toolkit
  layout, input, accessibility, style, text editing
        |
        v
backend-independent scene or display list
        |
        v
vector and raster renderer
        |
        +-> software raster backend
        +-> GPU backend
        +-> offscreen image backend
        +-> future print or document backend
        |
        v
application surface buffer submitted to the compositor
```

The toolkit does not own global window placement or sample other applications. The
renderer does not implement desktop policy. The compositor does not need to understand
application widget trees.

## Rendering engine

The renderer should provide a stable backend-independent scene representation for:

- lines, rectangles, rounded rectangles, ellipses, arcs, and arbitrary paths;
- cubic and quadratic curves;
- solid fills, strokes, dashes, joins, and caps;
- linear, radial, and later conic gradients;
- transforms, clips, masks, opacity groups, and blend modes;
- raster images and alpha masks;
- SVG scenes;
- positioned glyph runs and text decorations;
- shadows, blur, color matrices, and bounded filters;
- damage regions and offscreen layers.

A client should be able to build an immutable or incrementally updated display list
without targeting a specific GPU API.

Conceptually:

```rust
let mut scene = Scene::new();
scene.fill_rounded_rect(bounds, radius, background);
scene.stroke_path(path, border);
scene.draw_text(&glyphs, origin, foreground);
scene.draw_image(&image, destination, sampling);
```

This example is illustrative, not a frozen API.

## Immediate and retained use

The engine should support both:

- an immediate builder used to construct one frame or layer;
- retained resources and scene fragments that can be cached and reused across frames.

The toolkit may retain widget layout, style, text, and render nodes while rebuilding
only damaged portions. Custom drawing code should not require callers to manually
manage every cache object.

All object counts, path sizes, filter regions, image dimensions, and recursion must be
bounded or checked because scenes and assets may originate from untrusted packages.

## Software-first and accelerated backends

The first backend should be a correct software rasterizer that can render into the
existing framebuffer or shared-memory window buffers. It establishes reference
behavior for paths, clipping, alpha, text, and SVG before GPU differences complicate
testing.

A later GPU backend should add:

- cached path tessellation or another vector strategy;
- glyph and image atlases;
- batched primitives;
- retained textures and offscreen layers;
- damage-aware partial rendering;
- explicit buffer synchronization;
- color-space conversion and output metadata;
- bounded GPU memory accounting and recovery after device reset.

Applications should not depend on GPU-specific behavior for correctness. Unsupported
or excessively expensive filters may fall back to software or a documented simpler
result.

## Pixel, alpha, and color model

Internal compositing should use premultiplied alpha. Image and surface metadata should
name pixel format, alpha mode, transfer function, color space, and intended precision
rather than assuming all bytes are untagged sRGB.

The initial desktop may standardize on sRGB-like 8-bit output, but the API should leave
room for wide-gamut, floating-point intermediates, color profiles, HDR, and print
conversion.

Sampling policy should include nearest and linear modes initially, with higher-quality
resampling added where measurement justifies it.

## Raster images and codecs

The renderer consumes validated decoded image buffers. Complex or untrusted image
formats should eventually be decoded by sandboxed codec or thumbnail services rather
than inside the compositor or file manager.

Image objects should support:

- RGB, RGBA, grayscale, and alpha-only data;
- integer and later floating-point formats;
- immutable or explicitly updated content;
- logical size independent of pixel dimensions;
- color metadata;
- orientation and animation-frame sources where needed.

Package resources and explicitly granted file handles are valid image sources. Style
sheets must not fetch arbitrary filesystem or network URLs.

## SVG assets

SVG is the preferred scalable format for:

- application and system icons;
- symbolic toolbar and status icons;
- cursors;
- illustrations and empty-state artwork;
- toolkit and theme resources.

NullStar should define a safe supported SVG profile. The initial profile should cover
paths, basic shapes, groups, transforms, fills, strokes, gradients, opacity, clipping,
and embedded raster data. Masks and selected filters may follow.

Scripts, active content, external network resources, unrestricted local file access,
and unbounded filter regions are not allowed in system assets.

SVG should be parsed into an immutable validated scene representation and cached by
content identity, theme role, scale, and renderer backend.

## Symbolic icons

Symbolic icons should support semantic paint roles rather than one hard-coded color:

```text
foreground
secondary
accent
warning
destructive
disabled
```

A theme maps roles to actual colors. Full-color icons remain supported and are not
recolored unless their metadata opts into a defined transformation.

Icon lookup should integrate with the freedesktop compatibility layer while allowing
native package resources and semantic role metadata.

## SVG cursors

The cursor service and compositor should prefer scalable cursor sources with metadata
for:

- standard cursor name;
- SVG source;
- hotspot;
- nominal logical size;
- optional animation frames and timing;
- fallback raster assets.

The compositor rasterizes and caches the active cursor at each output scale. An
application requests a standard or package-provided cursor for its own surfaces; it
does not replace the global theme or inspect another client's cursor state.

## Native Rust toolkit

The toolkit should expose safe Rust ownership around:

- application runtime and windows;
- widget trees and stable widget identity;
- layout and measurement;
- input dispatch, focus, gestures, and commands;
- asynchronous tasks and messages;
- text layout and editing;
- accessibility trees;
- theme resolution and animation;
- compositor surfaces, buffers, and portals.

The first API may use an Elm-like message/update/view model, retained widgets, or a
careful hybrid. The architecture should not force one state-management pattern on
custom application code if safe interoperation can be preserved.

Most widgets are application-local scene nodes, not compositor surfaces. Windows,
popups, menus that require cross-window placement, and explicitly embedded external
surfaces use compositor objects.

## Widget identity

Every stylable widget should expose:

- a stable type name;
- an optional application-assigned ID;
- zero or more classes;
- documented pseudo-states;
- documented style parts;
- accessibility role, name, value, state, and actions.

Conceptually:

```rust
Button::new("Connect")
    .id("connect-button")
    .class("primary")
    .class("network-action")
```

A selector may address it as:

```text
button#connect-button.primary.network-action
```

IDs and classes are local to the application's widget tree. They do not provide a way
to style or inspect another process.

## Widget parts

Complex widgets should expose stable named parts rather than undocumented internal
children. Examples include:

```text
button::content
button::icon
button::label

slider::track
slider::fill
slider::thumb
slider::tick

scroll-view::viewport
scroll-view::scrollbar
scroll-view::thumb

text-input::field
text-input::placeholder
text-input::selection
text-input::cursor

window::background
window::header
window::title
window::content
window::resize-handle

menu::item
menu::icon
menu::label
menu::shortcut
menu::submenu-arrow
```

Part names are toolkit compatibility contracts. A theme should not depend on private
layout implementation.

## NullStar Style Sheets

The theme language should use familiar CSS syntax but be specified as a constrained
NullStar Style Sheet language. It is not browser CSS and does not include a DOM,
JavaScript, arbitrary URL loading, or every web layout algorithm.

Example:

```css
button {
    padding: 8px 14px;
    border-width: 1px;
    border-color: var(--border);
    border-radius: 8px;
    background: var(--button-background);
    color: var(--text-primary);
}

button.primary {
    background: linear-gradient(
        180deg,
        var(--accent-light),
        var(--accent)
    );
    color: var(--on-accent);
}

button:hover {
    box-shadow: 0 3px 12px rgba(0, 0, 0, 0.22);
}

slider::thumb {
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background-color: var(--accent);
}
```

## Style properties

The initial property set should include:

```text
background-color
background-image
linear-gradient
radial-gradient
border-color
border-width
border-radius
padding
margin
width, height, min-width, min-height, max-width, max-height
color
font-family
font-size
font-weight
font-style
text-align
line-height
letter-spacing
box-shadow
text-shadow
opacity
visibility
cursor
```

Toolkit-specific properties may include:

```text
icon-size
icon-color
spacing
row-spacing
column-spacing
focus-ring
focus-ring-offset
content-density
backdrop-material
transition
animation
```

Every property needs defined inheritance, initial value, unit behavior, invalid-value
handling, animation behavior, and layout or paint cost.

## Selectors and pseudo-states

The first selector model should support widget types, IDs, classes, parts, bounded
descendant or child relationships, and states such as:

```text
:hover
:active
:focus
:focus-visible
:disabled
:checked
:selected
:indeterminate
:dragging
:drop-target
:window-active
:window-inactive
:first-child
:last-child
```

Environment states may include high contrast, reduced motion, touch input, and keyboard
navigation. The selector engine must avoid pathological unbounded matching behavior.

## Theme variables and semantics

Themes should be based primarily on semantic variables:

```css
:root {
    --accent: #4f8cff;
    --background: #16181d;
    --surface: #20232a;
    --text-primary: #f5f7fa;
    --text-secondary: #b8bec8;
    --border: rgba(255, 255, 255, 0.12);
    --destructive: #e65353;

    --radius-small: 5px;
    --radius-medium: 9px;
    --radius-large: 14px;

    --spacing-small: 4px;
    --spacing-medium: 8px;
    --spacing-large: 16px;
}
```

Applications should prefer semantic roles to literal platform colors. This allows
system-wide dark/light modes, accent changes, contrast themes, density changes, and
accessibility adjustments.

## Cascade and application boundaries

The preferred cascade is:

```text
toolkit defaults
    -> system theme
    -> application style sheet
    -> widget classes and states
    -> explicit programmatic overrides
    -> system-enforced accessibility and security adjustments
```

Applications may style their own widget tree and package resources. They may not:

- modify another process or the global theme;
- style trusted permission, lock, or secure-attention surfaces;
- load executable code through a style sheet;
- read arbitrary files through asset URLs;
- request an effect that exposes other clients' pixels;
- suppress a required system focus indicator or accessibility override.

System and application style sheets should be parsed and validated before activation.
A bad application rule fails locally rather than destabilizing the desktop.

## Layout

The native layout system should provide a small predictable set of primitives:

```text
row
column
wrap/flow
grid
stack
overlay
scroll
absolute canvas
```

Rows and columns may use Flexbox-inspired sizing and alignment without promising full
web Flexbox behavior. Grid should likewise define only the subset needed by native UI.

Style may influence margins, padding, gaps, alignment, and size constraints. Application
logic retains ownership of semantic structure and virtualized data.

## Text system

Text needs a dedicated subsystem for:

- Unicode and UTF-8;
- shaping, bidirectional layout, scripts, and font fallback;
- line breaking, alignment, ellipsis, and selection;
- variable fonts, features, weight, and style;
- caret movement, editing, input methods, and composition;
- glyph caching and grayscale or subpixel policy;
- accessible text ranges, names, and actions.

The toolkit produces positioned glyph runs for the renderer. Fonts should be obtained
through a font service or authorized resources rather than unrestricted traversal of
system font files.

## Animation

The style system may animate documented numeric, color, transform, and effect
properties. Animations must honor reduced-motion policy, have bounded resource use, and
avoid creating one unbounded timer or allocation stream per widget.

Window movement, opacity, and backdrop effects may be compositor animations. Ordinary
widget animation remains in the application renderer.

## Backdrop materials

A style may request a semantic compositor material:

```css
window.translucent::background {
    background-color: rgba(24, 28, 36, 0.72);
    backdrop-material: blur(24px) saturate(1.15);
}
```

The toolkit sends a region and material request to the compositor. It never receives
the sampled backdrop. The compositor may reduce or replace the effect because of
protected content, performance, power, remote-session, or accessibility policy.

## Accessibility

Painting and semantics are separate. Every standard widget should expose role, name,
description, value, state, relationships, bounds, text ranges, and actions.

A custom-styled button remains a button to assistive technology. Themes cannot make a
widget semantically disappear while it remains interactive.

High-contrast policy may replace colors, remove transparency, strengthen borders,
disable shadows and blur, and enforce focus indicators. Reduced motion may remove or
shorten transitions without application-specific code.

## Recommended implementation stages

1. Implement a software renderer for paths, rounded rectangles, fills, strokes,
   clipping, transforms, alpha compositing, raster images, and basic text.
2. Add a safe SVG profile, symbolic icon roles, and scalable cursor rasterization.
3. Implement application surfaces plus row, column, stack, scroll, label, button,
   image, checkbox, list, and basic text-input widgets.
4. Add focus, keyboard navigation, pointer events, and the accessibility-tree
   foundation.
5. Specify and implement style variables, type/class/ID selectors, parts,
   pseudo-states, borders, backgrounds, gradients, spacing, text, and shadows.
6. Add live theme changes, animation, high-contrast and reduced-motion policy.
7. Add damage tracking, scene caching, glyph/image atlases, and an accelerated backend.
8. Add complex text, input methods, color management, richer SVG, printing, and
   document rendering.

## Open questions

- Public crate and module names.
- Retained widget tree, declarative view tree, or hybrid application API.
- Exact style grammar, cascade specificity, units, and layout subset.
- Initial text shaping and font stack.
- Software rasterizer implementation strategy and GPU backend API.
- Which filters and SVG features are safe and performant enough for the first profile.
- How custom widgets publish stable parts without freezing internal implementation.
