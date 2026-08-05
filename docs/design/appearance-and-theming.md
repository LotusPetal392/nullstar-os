# Appearance, theming, and display adaptation direction

## Status

Independently selectable appearance components joined by an appearance profile,
semantic system color variables, light and dark variants, state-driven theme
animation, system and per-user theme installation, and a dedicated session appearance
service are **accepted direction**. Widgets owning their semantic structure while
themes control presentation is also accepted direction.

Widgets should expose stable named paint surfaces, while the style language may add a
small fixed number of noninteractive decorative surfaces. Keeping night-light and
output color transformation in a display color service rather than the theme engine is
accepted direction.

The exact package suffixes, service and protocol names, semantic-token spelling,
manifest schema, number of generated decorative surfaces, style grammar, color
pipeline, and final package subdirectories remain **tentative design**.

This document extends the styling foundation in
[Native graphics renderer and UI toolkit](graphics-renderer-and-toolkit.md) and the
global security and output boundary in
[Graphics stack and compositor](graphics-stack.md).

## Core principles

The appearance system should follow these rules:

1. **Widgets own structure and semantics; themes own presentation.**
   A theme may restyle stable surfaces and states, but it may not redefine widget
   behavior, accessibility meaning, input handling, or required controls.
2. **An appearance profile coordinates independent components.**
   Users may select a coherent preset and then replace its icons, cursor, wallpaper,
   window decorations, panel, dock, or another component independently.
3. **Semantic tokens provide coherence.**
   Applications and shell components consume roles such as accent, surface, text,
   focus, selection, warning, and error instead of hard-coded palette values.
4. **Packages and policy remain separate.**
   A theme package supplies assets and defaults. User policy chooses light or dark
   mode, schedules, accent source, contrast, motion, transparency, and display
   adaptation.
5. **Display adaptation is not theming.**
   Night light, output profiles, HDR policy, calibration, and related final-output
   transforms belong to a display color service even when exposed in the same
   Settings application.
6. **Themes are untrusted data.**
   They are declarative, bounded, validated before activation, and unable to execute
   code or acquire ambient authority.
7. **Accessibility policy wins over theme intent.**
   Required focus indication, contrast, reduced motion, reduced transparency, cursor
   scaling, and secure-surface rules may replace theme choices.

## Service architecture

The preferred session architecture is:

```text
Settings application and session policy
                 |
                 v
        appearance service
          package discovery
          profile resolution
          variant and accent policy
          schedule evaluation
          accessibility overrides
                 |
                 v
       resolved appearance snapshot
          |          |           |
          v          v           v
 desktop shell   UI toolkit   session compositor
 panels/docks    applications decorations/materials/cursor

Settings application and display policy
                 |
                 v
       display color service
          output profiles
          calibration
          night light
          HDR and tone policy
                 |
                 v
        compositor output pipeline
```

The appearance service is a per-session policy service. It should:

- discover and validate installed appearance packages;
- resolve profile component references, inheritance, and fallbacks;
- select a light, dark, or contrast variant;
- apply the selected accent source and derive related semantic colors;
- apply accessibility and power-policy overrides;
- evaluate fixed and solar schedules;
- publish one immutable, generation-numbered appearance snapshot;
- coordinate transactional previews and recovery fallback;
- notify the shell, compositor, toolkit, and interested applications of changes.

The display color service should own output-specific color policy and provide the
compositor with validated transforms and transition schedules. The exact service
identities are tentative; illustrative names are `appearance.session` and
`display.color`.

Clients should consume a resolved snapshot or typed token table rather than parsing
arbitrary theme packages independently. This keeps resolution deterministic and avoids
each application implementing a different cascade or dependency policy.

## Appearance profiles

An **appearance profile** is the user-facing coherent preset. It references separately
installed components rather than combining every asset into one inseparable theme.

Candidate component roles are:

```text
color scheme
widget theme
window-decoration theme
panel theme
dock theme
menu and popup theme
icon theme
cursor theme
wallpaper set
background effect
sound theme
motion profile
```

A package may supply one component, several related components, or a complete profile.
Missing components fall back through the profile parent, system default profile, and
finally an embedded recovery appearance.

Illustrative profile data:

```toml
[profile]
id = "org.nullstar.appearance.aurora-dark"
name = "Aurora Dark"
version = "1.0.0"

[components]
color_scheme = "org.nullstar.colors.aurora"
widgets = "org.nullstar.theme.aurora"
windows = "org.nullstar.theme.aurora"
panels = "org.nullstar.theme.aurora"
docks = "org.nullstar.theme.aurora"
icons = "org.nullstar.icons.nova"
cursors = "org.nullstar.cursors.nova-light"
wallpaper = "org.nullstar.wallpaper.aurora-night"
background_effect = "org.nullstar.effect.slow-parallax"
motion = "org.nullstar.motion.standard"

[defaults]
variant = "dark"
accent_source = "theme"
```

The profile's defaults are suggestions. User policy may override any independently
selectable component without copying or modifying the profile package.

A settings application should describe such a result as, for example:

```text
Profile: Aurora Dark
Icons: High Contrast (user override)
Cursor: Large White (accessibility override)
Variant: Dark (solar schedule)
Accent: Wallpaper-derived
```

## Package model and installation scope

Every appearance package should have a stable globally scoped identifier, display name,
version, package type, theme API level, compatibility range, supplied variants, assets,
and dependencies.

Illustrative manifest:

```toml
[package]
id = "org.nullstar.theme.aurora"
name = "Aurora"
version = "1.2.0"
type = "visual-theme"

[compatibility]
theme_api = 1
minimum_os_version = "0.8.0"

[components]
widgets = "widgets.nss"
windows = "windows.nss"
panels = "panels.nss"
docks = "docks.nss"
animations = "motion.nss"

[variants]
light = "colors/light.nss"
dark = "colors/dark.nss"
high_contrast_light = "colors/high-contrast-light.nss"
high_contrast_dark = "colors/high-contrast-dark.nss"

[assets]
root = "assets"

[dependencies]
color_scheme = "org.nullstar.colors.aurora"
```

System packages and user packages should live in distinct scopes. The recommended
logical roots are:

```text
/System/Appearance/
├── Profiles/
├── Themes/
├── ColorSchemes/
├── Icons/
├── Cursors/
├── Wallpapers/
├── BackgroundEffects/
├── Sounds/
└── MotionProfiles/

/Users/<name>/Profile/data/appearance/
├── Profiles/
├── Themes/
├── ColorSchemes/
├── Icons/
├── Cursors/
├── Wallpapers/
├── BackgroundEffects/
├── Sounds/
└── MotionProfiles/
```

The exact capitalization and subdirectory names remain tentative, but system defaults
must be below `/System`, and user-installed packages must be durable profile data rather
than hidden dot directories. User preferences belong below `Profile/config`, resolved
indexes and rasterized assets below `Profile/cache`, and temporary preview state below
`Profile/runtime`.

The lookup order is:

```text
explicit user-selected package
    -> matching user-scope package
    -> matching system-scope package
    -> system default
    -> embedded recovery fallback
```

A user package must not silently impersonate a system package by reusing its identifier.
The resolver should reject the collision or require an explicit scope-qualified
selection.

Theme inheritance is useful for small variations:

```toml
[theme]
id = "org.example.aurora-purple"
extends = "org.nullstar.theme.aurora"

[overrides]
colors = "purple-overrides.nss"
```

Inheritance must have a small maximum depth, detect cycles, validate compatibility, and
resolve to one immutable result before activation.

## Semantic appearance tokens

NullStar should define a stable semantic token set available to native widgets, shell
components, and system-aware custom renderers. Exact token names remain tentative, but
the roles should include at least:

```text
accent
accent-hover
accent-active
on-accent
surface-base
surface-primary
surface-secondary
surface-elevated
surface-overlay
text-primary
text-secondary
text-disabled
border-subtle
border-normal
border-strong
focus-ring
selection-background
selection-foreground
success
warning
error
information
window-background
window-active-border
window-inactive-border
panel-background
dock-background
```

A NullStar Style Sheet may expose the resolved roles as CSS-like variables:

```css
:root {
    --system-accent: #6fa8ff;
    --system-on-accent: #07111f;

    --system-surface-base: #101218;
    --system-surface-primary: #171a22;
    --system-surface-elevated: #292e39;

    --system-text-primary: #f4f6fb;
    --system-text-secondary: #b8bfcc;
    --system-text-disabled: #737b89;

    --system-border-subtle: rgba(255, 255, 255, 0.08);
    --system-border-normal: rgba(255, 255, 255, 0.16);
    --system-focus-ring: var(--system-accent);
}
```

Themes may define private variables, but applications should depend only on the stable
semantic set. Component themes may alias global roles into component-local values:

```css
panel {
    --component-background: var(--system-panel-background);
    --component-highlight: var(--system-accent);
}
```

The user-selectable accent source should support:

```text
theme default
custom user color
wallpaper-derived color
```

An application may use its own local brand accent inside its surfaces, but it cannot
replace the session-global accent. Wallpaper extraction should be asynchronous, cached,
bounded, and produce a deterministic fallback.

The resolver may derive hover, active, muted, and foreground colors using a constrained
set of color functions such as mixing, alpha adjustment, lightness adjustment, and
contrast-color selection. It should validate minimum contrast for required text and
focus roles and either warn, adjust, or fall back when a custom accent is unreadable.

Semantic tokens should carry an explicit color space rather than assuming that every
future value is an untagged sRGB byte tuple.

## Variants and appearance policy

A color scheme may supply:

```text
light
dark
high-contrast light
high-contrast dark
optional dim
optional OLED dark
```

The appearance service resolves the active variant from package support and user policy.
Applications should not need to restart when the variant changes. Instead, the service
publishes a new snapshot and toolkits restyle existing widget trees atomically.

Illustrative change event:

```text
AppearanceChanged {
    generation
    profile_id
    color_scheme_id
    variant
    accent
    contrast_policy
    motion_policy
    transparency_policy
}
```

The event carries data and identifiers, not package filesystem authority.

User appearance policy should include:

```text
variant policy:
    manual
    fixed schedule
    sunrise and sunset
    future ambient-light sensor

accent source:
    theme
    custom
    wallpaper

motion:
    full
    reduced
    essential only
    none

transparency:
    full
    reduced
    opaque

contrast:
    normal
    increased
    high contrast
```

Applications may query these policies even when they intentionally use a branded
interface.

## Scheduled light and dark mode

The first scheduling modes should be manual, fixed schedule, and sunrise/sunset.

Illustrative fixed schedule:

```toml
[appearance.variant]
policy = "schedule"
light_at = "07:00"
dark_at = "19:30"
```

Illustrative solar schedule:

```toml
[appearance.variant]
policy = "sun"
day_variant = "light"
night_variant = "dark"
sunrise_offset_minutes = 15
sunset_offset_minutes = -20
```

Solar scheduling should work from a user-selected city, coarse geographic region, or an
explicit grant to the system location service. Continuous precise location access must
not be required. Sunrise and sunset may be calculated and cached daily.

The appearance service should transition at a stable wall-clock boundary, handle time
zone and daylight-saving changes, and avoid repeatedly toggling when the clock moves
backward. A login after the scheduled boundary should immediately select the correct
variant.

Light/dark scheduling and night-light scheduling may use the same clock and solar
calculation library, but they remain independently enabled policies.

## Widget styling contracts

Each standard widget should publish a versioned styling contract containing:

- stable widget type, class, ID, and pseudo-state behavior;
- required and optional semantic surfaces;
- paint order and clipping rules;
- which properties may affect measurement, layout, paint, or composition;
- which surfaces may extend outside widget bounds and by how much;
- accessibility and trusted-UI restrictions;
- animation and invalidation behavior.

For a button, candidate semantic surfaces are:

```text
button::outer-shadow
button::background
button::highlight
button::content
button::icon
button::label
button::state-overlay
button::default-indicator
button::border
button::focus-indicator
```

Not every widget exposes every surface. Unsupported optional surfaces are ignored or can
be tested through a capability query. Private implementation children are not styling
contracts.

Semantic surfaces are the primary mechanism because the widget knows its structure,
clipping, layout, focus behavior, disabled behavior, hit testing, and accessibility
semantics. A theme should not reconstruct those responsibilities from arbitrary
generated objects.

Illustrative styling:

```css
button::background {
    background: linear-gradient(
        to bottom,
        var(--button-top),
        var(--button-bottom)
    );
    border-radius: inherit;
}

button::highlight {
    inset: 1px 2px 52% 2px;
    border-radius: inherit;
    background: linear-gradient(
        to bottom,
        rgba(255, 255, 255, 0.60),
        transparent
    );
}

button::content {
    color: var(--system-text-primary);
}

button::focus-indicator {
    border: 2px solid var(--system-focus-ring);
}
```

## Generated decorative surfaces

Only predefined widget surfaces would make the engine predictable but unnecessarily
restrict expressive themes. Unlimited CSS-generated objects would make layout,
accessibility, optimization, and security unpredictable. NullStar should therefore use
a hybrid model.

A theme may create a small fixed number of paint-only decorative surfaces. CSS-like
`::before` and `::after` names are the preferred initial syntax:

```css
button::before {
    layer: below-content;
    inset: 1px 2px 50% 2px;
    border-radius: inherit;
    background: linear-gradient(
        to bottom,
        rgba(255, 255, 255, 0.70),
        rgba(255, 255, 255, 0.08)
    );
}

button::after {
    layer: above-content;
    inset: auto 15% 2px 15%;
    height: 3px;
    background: var(--system-accent);
    opacity: 0;
    filter: blur(3px);
}
```

Generated decorative surfaces:

- are display-list or scene nodes, not child widgets;
- never receive pointer, keyboard, gesture, drag, or accessibility events;
- never appear in the accessibility tree;
- cannot add text labels or change localization;
- cannot alter intrinsic size, measurement, or ordinary layout;
- cannot contain arbitrary widgets or executable content;
- use only package-local validated assets;
- are clipped to widget-defined regions unless a small overflow extent is allowed;
- use fixed semantic layer slots rather than arbitrary `z-index`;
- count against per-widget layer, filter, memory, and animation limits;
- cannot obscure, move, or imitate required trusted controls.

Candidate layer slots are:

```text
above-background
below-content
above-content
below-border
```

The first implementation may expose only `below-content` and `above-content`. The
toolkit clamps geometry, filter radius, transforms, and overflow to the widget contract.

Web-style textual `content` should not be supported. Decorative package assets may be
allowed, but labels, icons with semantic meaning, badges, and control content belong in
real widget surfaces.

A candidate default paint order is:

```text
1. outer shadow
2. widget background
3. generated below-content surface
4. widget highlight
5. content
6. state overlay
7. generated above-content surface
8. border
9. focus indicator
```

Individual widget contracts may add named slots, but their order must be documented and
stable for a theme API generation.

## State-driven animation

Theme animation should be state-driven rather than programmable. The toolkit or
compositor owns interaction state and animation scheduling; the theme owns the visual
response.

Important states include:

```text
:hover
:pressed
:checked
:selected
:disabled
:focus-visible
:default
:indeterminate
:dragging
:drop-target
:window-active
:window-inactive
:attention
```

`:default` is distinct from keyboard focus. A text field may own focus while a dialog's
default action button remains the control invoked by Enter. This distinction permits an
Aqua-style default button pulse without misrepresenting focus.

Illustrative theme animation:

```css
@keyframes default-button-pulse {
    0%, 100% {
        opacity: 0.38;
        transform: scale(0.99);
        filter: brightness(1.00);
    }

    50% {
        opacity: 0.72;
        transform: scale(1.00);
        filter: brightness(1.08);
    }
}

button:default::default-indicator {
    background: var(--system-accent);
    filter: blur(5px);
    animation: default-button-pulse 1.8s ease-in-out infinite;
}
```

The default indicator can also change shadow intensity or tint while the button
background and content remain retained.

The initial animatable property set should be limited to documented color, opacity,
transform, shadow, outline, gradient-position, brightness, saturation, blur, and
bounded progress values. Themes should not animate input regions, security state,
arbitrary layout, unbounded filter regions, resource loading, or executable shaders.

Window-level focus, open, close, minimize, workspace, panel, dock, and overview
animations may run in the compositor. Ordinary button, selection, toggle, menu, and
scroll animations run in the client toolkit. A style rule must not cause the compositor
to inspect an application's widget tree.

Continuous animations should:

- pause when their window or surface is hidden or minimized;
- stop when the owning process or widget generation disappears;
- share clocks where practical rather than creating one timer per widget;
- avoid repeated layout and full-window repaint;
- respect frame-rate, power, thermal, and remote-session policy;
- degrade to a static state if the renderer cannot implement them safely.

## Motion and accessibility overrides

Reduced motion is not one binary preference. Internally, the system should distinguish:

```text
short state transitions
continuous ambient animation
large spatial movement
flashing or rapid luminance change
```

A user may allow brief button transitions while disabling continuously pulsing controls
or large workspace movement.

Themes should be able to provide explicit alternatives:

```css
@media (prefers-reduced-motion) {
    button:default::default-indicator {
        animation: none;
        opacity: 0.65;
        border: 2px solid var(--system-focus-ring);
    }
}

@media (prefers-reduced-transparency) {
    panel,
    dock,
    menu {
        backdrop-material: none;
        background: var(--system-surface-elevated);
    }
}
```

System policy must still enforce a safe fallback when a theme does not supply one.
High-contrast policy may replace colors, remove gradients, increase border width,
disable shadows and blur, or force a focus indicator. Cursor size and contrast are user
settings, not theme vetoes.

## Application participation

Applications fall into three appearance modes:

1. **System themed.** Standard toolkit widgets consume the resolved profile and semantic
   tokens automatically.
2. **System aware custom rendering.** A custom renderer consumes variant, accent,
   surface, text, contrast, motion, transparency, cursor, and text-scale policy.
3. **Intentionally branded.** Games, media tools, and specialized creative software may
   use a distinct interface while still respecting accessibility settings, trusted
   system dialogs, cursor scale, and user-selected motion policy.

An application manifest may describe its behavior:

```toml
[appearance]
mode = "custom"
respects_system_variant = true
respects_system_accent = false
respects_reduced_motion = true
respects_reduced_transparency = true
```

This declaration documents intent; it does not grant authority to ignore mandatory
accessibility or secure-surface rules.

Applications may style only their own widget trees and package resources. They cannot
install a session theme implicitly, modify another application, inspect another
client's resolved private styles, or style trusted permission and authentication
surfaces.

## Wallpaper and background effects

Wallpaper selection should support per-output and, later, per-workspace assignment.
Static images, image sets, time-of-day sets, and system-provided animated backgrounds
are reasonable component types.

Background effects should initially be system implementations with bounded declarative
parameters:

```toml
[effect]
implementation = "parallax"
intensity = 0.35
blur = 0.15
particle_density = 0.10
```

This permits safe effects without loading arbitrary native code or unrestricted shaders
into the shell or compositor. Custom validated shader packages are a distant goal and
would require a restricted language, resource budgets, deterministic compilation,
device-reset handling, and a way to prevent capture or sampling of protected content.

Background animation should honor reduced motion, power saving, output refresh, remote
desktop, and session-lock policy. The compositor may substitute a static frame.

## Night light and output color policy

Night light should be implemented by the display color service and compositor output
pipeline, not by rewriting theme colors or application buffers.

Illustrative policy:

```toml
[display.night_light]
enabled = true
policy = "sun"
temperature_kelvin = 3800
transition_minutes = 30
```

A fixed schedule should also be supported:

```toml
[display.night_light]
policy = "schedule"
start = "21:00"
end = "07:00"
temperature_kelvin = 3400
```

The output transform should:

- apply per output;
- transition smoothly;
- coexist with ICC profiles, calibration, wide gamut, HDR, and tone mapping through a
  defined color pipeline;
- avoid changing application-owned source buffers;
- avoid baking the tint into ordinary screenshots or application window capture;
- allow a capture API to request the final displayed result only through explicit
  user-visible policy;
- degrade predictably on hardware that lacks the preferred transform mechanism.

Color-critical applications may request a temporary user-approved exemption or
reference-output mode. They cannot silently disable a user-selected night-light policy
for the whole display.

Brightness, ambient-light response, calibration, ICC assignment, HDR mode, and night
light may share one settings page, but they remain display policy rather than theme
assets.

## Runtime changes and transactional preview

Activating an appearance should be transactional:

1. validate all manifests, assets, style sheets, dependencies, and limits;
2. resolve the complete profile and fallback graph;
3. build an immutable appearance snapshot with a new generation;
4. apply it to a temporary preview session;
5. require confirmation or automatically revert;
6. commit user configuration only after successful confirmation.

A crash, unreadable theme, missing cursor, invalid stylesheet, or disconnected settings
application must revert to the previous confirmed generation. The system must retain an
embedded recovery appearance that does not depend on user packages, GPU-only effects,
or optional fonts.

The compositor, shell, and toolkit should switch generations atomically enough to avoid
mixing old colors with new component assets for an extended period. Slow resources may
be prepared before publication.

A live update must not require applications to reopen windows. Clients that do not
understand a newer optional token retain compatible defaults.

## Security model

Appearance packages are untrusted content even when installed for one user. They must
not be able to:

- execute native, bytecode, script, or shell code;
- load network resources;
- read arbitrary local files;
- inspect window titles, documents, pixels, input, or application identity;
- create interactive invisible overlays;
- alter hit testing or input routing;
- add misleading text to controls;
- suppress required warnings, denial actions, focus indicators, or accessibility state;
- claim trusted lock-screen, authentication, permission, or secure-attention roles;
- request unbounded images, paths, filters, recursion, animation layers, or allocations.

Lock screens, authentication prompts, permission dialogs, disk-encryption UI, and secure
attention surfaces should use a restricted trusted theme subset or a system-controlled
appearance. A user palette may influence safe roles such as accent or contrast, but the
theme cannot remove required structure or make a secure prompt indistinguishable from
ordinary application content.

Package parsing and image decoding should use the same bounded and sandboxed asset rules
as the renderer. A malformed theme should fail locally and leave the last confirmed
appearance active.

## Recommended implementation stages

1. Define the semantic token schema, appearance snapshot, generation event, and
   profile/component data model.
2. Add system and user package discovery, manifests, validation, fallback, and static
   light/dark variants.
3. Integrate standard widget semantic surfaces with the NullStar Style Sheet cascade.
4. Add icon, cursor, wallpaper, panel, dock, and window-decoration component packages.
5. Add state transitions, the `:default` state, generated decorative surfaces, and
   accessibility motion/transparency overrides.
6. Add transactional preview, fixed schedules, solar calculation, and wallpaper-derived
   accent selection.
7. Add per-output display color service integration, night light, output profiles, and
   smooth transitions.
8. Add richer motion profiles, animated backgrounds, advanced color management, and
   carefully validated extensibility only after resource accounting is mature.

## Open questions

- Final appearance and display service identities and wire protocols.
- Exact package suffixes, manifest encoding, directory names, and signature policy.
- Stable semantic-token names, color spaces, and contrast-adjustment rules.
- Theme API compatibility and how long old widget surface contracts remain supported.
- Maximum inheritance depth and package dependency rules.
- Whether the initial generated surfaces are exactly `::before` and `::after`.
- The final set of layer slots, geometry limits, and allowed animatable properties.
- Whether semantic animation roles supplement or replace some direct keyframe usage.
- Solar-location source, privacy UI, and behavior when location is unavailable.
- The exact ordering of calibration, HDR/tone mapping, night light, and output encoding.
- Whether user-authored validated shaders are valuable enough to justify their security
  and stability cost.
- The scope of per-application appearance overrides and how they interact with branded
  interfaces.
