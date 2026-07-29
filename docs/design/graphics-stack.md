# Graphics-stack and compositor direction

## Status

A compositor-controlled surface model, strict cross-application isolation, configurable
panel or dock hosts on every screen edge, and process-isolated panel applets are
**accepted direction**. A native NullStar display protocol with selected Wayland
compatibility is accepted direction. Exact wire protocols, layer names, GPU interfaces,
and shell/compositor process boundaries remain **tentative design**.

## Core architecture

The long-term graphical path is:

```text
applications and desktop components
        |
        v
native display protocol or Wayland compatibility frontend
        |
        v
session compositor
        |
        v
display-device and graphics protocols
        |
        v
GPU/display driver
```

The compositor is the trusted enforcement boundary for:

- surface ownership, roles, placement, visibility, and occlusion;
- buffer import, release, synchronization, and presentation;
- keyboard, pointer, touch, and focus routing;
- output layout, scaling, color policy, and work areas;
- window-level effects, decorations, and animations;
- clipboard, drag-and-drop, capture, and global-shortcut brokerage;
- secure surfaces such as lock screens and trusted permission prompts.

Window-management and desktop-shell policy may begin inside the compositor for
simplicity. Protocols should allow them to become supervised components later without
weakening the compositor's security role.

## Surface isolation

A normal client receives handles only for its own surfaces and buffers, input directed
to those surfaces, and explicitly granted desktop services. It must not be able to:

- enumerate arbitrary windows or inspect another application's metadata;
- learn another window's precise global position without an authorized service;
- map, read, or copy another client's buffers;
- observe input delivered to another client;
- inject trusted input;
- capture an output, window, or region without portal authorization;
- obtain clipboard data before a user-mediated transfer;
- create a panel, lock screen, permission prompt, or secure overlay by assigning its
  own role.

Every surface is attributed to a process generation, application or service identity,
job, session, and security context. Surface handles must become invalid when their
owning client generation ends.

Applications render into shared buffers and commit content updates. The compositor
chooses placement and presentation. Client APIs should not require exposure of the
global framebuffer.

## Surface roles

A surface receives one stable role. Candidate native roles include:

```text
toplevel
dialog
popup
tooltip
notification
desktop
panel
dock
applet
lock-screen
secure-prompt
screen-overlay
drag-icon
cursor
```

Roles define stacking, focus, placement authority, parent relationships, allowed child
surfaces, task-switcher visibility, capture policy, and whether screen space may be
reserved. An ordinary toplevel cannot later transform itself into a trusted role.

## Scene organization

The compositor should own a clear scene order. The current preferred model is:

```text
secure
  overlay
    top
      application
    bottom
  background
```

- **background**: wallpaper and noninteractive desktop visuals;
- **bottom**: desktop widgets or shell content beneath ordinary windows;
- **application**: toplevels, dialogs, popups, and application subsurfaces;
- **top**: panels, docks, launchers, notifications, and shell surfaces;
- **overlay**: transient shell UI, drag feedback, selectors, and volume or brightness
  displays;
- **secure**: lock, authentication, and trusted attention surfaces unavailable to
  ordinary clients.

These are NullStar scene bands, not a promise to reproduce one external layer-shell
protocol exactly. A Wayland compatibility frontend may map supported layer-shell roles
into the closest authorized native role.

## Backdrop blur and effects

A client that requests transparency or blur must not receive the pixels behind its
window. The client submits an effect request describing a region and semantic material,
for example blur, tint, saturation, opacity, or shadow.

```text
scene behind the window
        |
        v
compositor-owned backdrop sample
        |
        v
compositor blur, tint, and material pass
        |
        v
client surface composited above the result
```

The intermediate backdrop texture remains compositor-owned and is never mapped into
the requesting process. This prevents blur from becoming a covert screenshot API.

The compositor may clamp, simplify, or replace an effect because of performance,
power, accessibility, remote-session, or security policy. Protected content is
excluded from backdrop sampling and may appear as an opaque or sanitized region.

The native toolkit may request a semantic backdrop material, but it does not implement
global backdrop sampling itself.

## Protected content

A surface may request or be assigned protection from:

- screenshots and screen recording;
- screen sharing;
- workspace previews and thumbnails;
- backdrop sampling;
- remote-desktop export.

The compositor, portal, and user policy decide whether protection is mandatory,
optional, or unavailable. Secure prompts are always protected and must remain visually
distinguishable from application-drawn imitations.

## Screen edges, panels, and docks

Every output exposes four edge slots:

```text
Top:    panel | dock | none
Bottom: panel | dock | none
Left:   panel | dock | none
Right:  panel | dock | none
```

An edge host negotiates:

- preferred, minimum, and maximum thickness;
- whether it reserves application work area;
- overlapping, auto-hide, and reveal behavior;
- output and workspace scope;
- animation and input regions;
- orientation and scale.

The compositor computes usable work areas from authorized exclusive reservations. A
normal application cannot reserve a screen edge.

Panel and dock configuration should remain data owned by the desktop session, not
hard-coded compositor policy.

## Panels and docks as nested compositors

A panel or dock should be both a client of the session compositor and a restricted
compositor for its applets:

```text
session compositor
        |
        v
panel or dock surface and compositor process
        |
        +-> clock applet process
        +-> network applet process
        +-> media applet process
        +-> third-party applet process
```

Each applet runs in its own process or application job. It receives a display endpoint
scoped to the panel host and cannot inspect sibling applet buffers, unrelated windows,
or the global desktop.

An applet crash should remove or replace only its slot. The panel remains running, and
the service manager may restart the applet with bounded backoff. A panel crash should
not terminate applications or the session compositor.

Applets request popups through the panel. The panel and session compositor cooperate to
create a correctly placed transient surface while retaining the originating applet's
identity and permissions.

Appearing in a trusted panel does not grant an applet the panel's authority. Volume,
network, power, and similar applets receive narrow client capabilities to their
respective services.

## Input and focus

The input path should be:

```text
input driver -> input service -> session compositor -> focused surface
```

The compositor controls focus, pointer confinement, grabs, touch sequences, and secure
attention. Global shortcuts are registered through a broker with conflict resolution,
application identity, session lifetime, and user-visible authorization.

Invisible input-capture surfaces, background key logging, and unrestricted synthetic
input are not normal client capabilities.

Input methods, accessibility services, remote control, and testing tools require
specialized narrowly scoped protocols rather than one universal input-injection right.

## Clipboard and drag-and-drop

Data transfer is offer-based and explicit:

```text
source offers content types
        |
        v
compositor or data broker records the offer
        |
        v
user paste or drop action selects a destination
        |
        v
selected content stream is transferred
```

The destination receives only the chosen stream or file capabilities. Background
applications cannot poll arbitrary clipboard contents. Clipboard history is a separate
permissioned service.

Cross-application file transfer should integrate with portals so a sandbox receives
access to the selected files, not the source application's broader directory tree.

## Screen capture and remote desktop

Capture uses trusted portals:

1. an application requests an output, window, or region stream;
2. the compositor presents a trusted selector and policy UI;
3. the user chooses the source and persistence scope;
4. the application receives a restricted media-graph or frame-stream endpoint;
5. protected and secure surfaces are excluded or sanitized.

Permissions distinguish one-time screenshots, one window, one output, a selected
region, persistent screen sharing, and remote input.

## Native protocol and Wayland compatibility

NullStar should define a native capability-aware protocol whose objects map naturally
to handles, shared buffers, asynchronous events, and service identity. It should take
inspiration from Wayland's asynchronous object and committed-surface model without
making libwayland or Linux file descriptors part of the native ABI.

Wayland compatibility may be implemented as a frontend inside the compositor initially
or as a supervised compatibility service later. It translates supported Wayland
objects into the same native compositor decisions and does not bypass NullStar policy.

High-value compatibility targets include:

- core display, registry, compositor, surface, buffer, seat, and output behavior;
- shared-memory buffers;
- `xdg-shell` toplevel and popup roles;
- presentation timing;
- viewport and fractional scaling;
- relative pointer and pointer constraints;
- text input and input methods;
- explicit buffer synchronization and DMA-BUF-style accelerated sharing when the
  graphics driver model can enforce ownership;
- selected decoration, activation, session-lock, and capture protocols after their
  security mapping is defined.

Protocols exposing foreign toplevels, virtual input, output control, screencopy,
data-control, or global shortcuts must be advertised only through explicit policy.
Implementing an external protocol name never grants authority by itself.

An optional rootless X compatibility server is a distant goal and should run as a
confined compositor client.

## Scheduling and presentation

The compositor belongs to the interactive scheduling class with fast input and
presentation wakeups. It should not receive unlimited realtime priority. The bounded
realtime media graph may briefly preempt it to meet an audio quantum deadline.

The display protocol should support damage, frame callbacks, presentation feedback,
buffer release, scale, color metadata, and later target or commit timing. These allow
smooth rendering without busy polling.

The compositor should use direct scanout only when security, capture, effects, color,
and synchronization policy allow it.

## Accessibility

Every native widget and window must expose semantic accessibility data independent of
its pixels. The compositor and accessibility services need protocols for focus,
window identity, bounds, actions, text, magnification, contrast, reduced motion, and
assistive input.

A visually custom application remains isolated, but it may not hide required trusted
permission UI or remove system-enforced accessibility accommodations.

## Recommended implementation stages

1. Define native surface, buffer, role, commit, damage, and input-event objects.
2. Build a software-composited single-output session over the existing framebuffer.
3. Add ordinary toplevels, dialogs, popups, focus, and basic clipboard offers.
4. Add output work areas and one nested panel compositor with isolated applet jobs.
5. Add all four configurable edge slots, docks, auto-hide, popup forwarding, and
   service-manager integration.
6. Add capture, file-transfer, global-shortcut, and sensitive-input portals.
7. Add compositor-owned backdrop materials and protected-content policy.
8. Add accelerated buffers, explicit synchronization, modesetting, color management,
   and multi-output support.
9. Add a Wayland compatibility frontend beginning with core protocol and `xdg-shell`.
10. Add broader freedesktop, toolkit, accessibility, remote-desktop, and optional X
    compatibility.

## Open questions

- Exact native object and wire encoding.
- Whether shell policy is initially linked into the compositor or a privileged client.
- The long-term mapping between native scene roles and external layer-shell protocols.
- Buffer-allocation and synchronization contracts before a mature GPU driver exists.
- How protected-content policy interacts with user-requested capture and remote access.
- Whether panel applet popups are composited by the panel or always promoted to the
  session compositor.
