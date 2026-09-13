# Application portal picker renderer

## Status

The allocation-free picker renderer, ARGB software surface, authenticated input router, and bounded
compositor session registry are implemented. The portal-process event loop and application-manager
lifecycle wiring are separate future increments.

## Trust boundary

The picker does not accept a pathname, object identity, display list, or pixels from the requesting
application. It renders only entries already admitted by the authenticated provider-backed picker.
Navigation continues to use picker slot indices, so presentation code cannot introduce ambient path
lookup or `..` traversal.

The renderer also does not map the boot framebuffer. It emits rectangles and byte-oriented text
through `PortalPickerRenderTarget`. `PortalArgbSurface` implements that interface over bounded
compositor-owned ARGB8888 memory and includes a clipped 5-by-7 software font. Filesystem names remain
byte strings; unsupported bytes render with a replacement glyph instead of being reinterpreted as
trusted markup.

`PortalPickerInputRouter` accepts input only for a registered picker-session/surface pair from the
configured nonzero compositor process ID. Each surface has a strictly increasing event sequence;
duplicate or reordered events fail as replays. Key releases, pointer releases, secondary clicks,
zero scroll, and clicks outside a visible authenticated row are consumed without changing picker
state. Single primary clicks select visible rows and double clicks activate them.

## Implemented behavior

- At most 16 picker sessions exist concurrently, and nonzero monotonically allocated session IDs
  are not reused while live.
- Each session owns its picker, selected row, scroll position, visible-row count, and terminal state.
- Surface geometry is checked before any drawing. Visible rows are derived from bounded dimensions
  and capped by the picker's 32-entry limit.
- Render output includes an operation-specific heading, file/directory labels, selected-row
  treatment, and keyboard guidance.
- Up, down, page-up, and page-down navigation clamps at the authenticated entry bounds and keeps the
  selected row visible.
- Activating a directory clears the old listing through `enter_directory` and requests provider page
  cookie zero. Back navigation does the same through the bounded picker history.
- Activating or confirming a file reuses `select_entry`, preserving operation/kind checks. Directory
  confirmation selects the displayed directory through `select_current_directory`.
- Selection and cancellation are terminal UI states. Further input fails closed until the process
  integration removes the session.
- Incomplete provider listings produce an explicit request for the picker's next authenticated page
  cookie.
- ARGB surface construction validates dimensions, stride, multiplication, and backing-buffer size;
  all rectangle and glyph writes clip to the visible surface.
- Keyboard commands and pointer rows translate into typed picker events only after sender, session,
  surface, and event-sequence authentication.

## Process-integration contract

The future portal process must bind a session ID to the owned
`PendingApplicationPortalRequest`. A `Selected` outcome supplies only the already validated resource
identity and optional visible-entry index; it does not grant authority. The process must still call
the existing prepared-selection and durable-completion path on the matching pending request. A
`Cancelled` outcome must consume that same request with a terminal cancellation response.

The compositor must kernel-stamp its sender process ID onto raw input envelopes before passing them
to the input router. Provider page replies must continue through `application_portal_filesystem`
before reaching the session.

## Remaining work

1. Bind renderer sessions, provider requests, pending reply capabilities, and application-manager
   lifecycle in the standalone portal process.
2. Add the crash-injection acceptance gate around transfer and durable permission publication.
3. Extend input translation with accessibility actions after that protocol is defined.
