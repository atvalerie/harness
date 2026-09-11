# antislop-layoutmobile

Mobile and responsive layout skill for antislop: reflow, breakpoints, scale, grids, overflow, and tap targets.

## Core Principle
Mobile layout is a distinct layout, not a desktop layout shrunk down. It must reflow: restack, rescale, and re-order with intent.

## Breakpoints & Reflow

### No Rigid Device-Specific Breakpoints
- Do not set breakpoints based on specific phone models (e.g. 375px, 414px). Place breakpoints where the content actually breaks or feels crowded.
- Avoid two-state-only designs (single mobile column vs huge desktop grid). Accommodate the middle tablet/small laptop range (600px–1024px) with multi-column reflow (e.g., 1 col -> 2 col -> 3/4 col).

### Mobile as a Deliberate State
- Do not treat mobile as a small CSS override appended to a 1000-line desktop stylesheet. Treat narrow viewports with intentional hierarchy, sizing, and spacing.

## Scale & Sizing

### Proportionate Spacing & Typography
- Scale down section padding and container gaps on mobile (e.g., reduce 96px/128px section margins to 32px/48px).
- Use fluid typography (`clamp()`) or smaller heading scales on small viewports so headers do not take over entire screens.
- Avoid fixed `100vh` on mobile where browser toolbars cause content clipping; use `100dvh` or content-driven `auto` heights.

## Grids & Navigation

### Collapsible Columns
- Multi-column card grids must collapse to single columns or scrollable carousels before text wraps into unreadable vertical slivers.
- Avoid horizontal overflow (`overflow-x: hidden` is not a fix for a broken layout; fix the element causing unintended width expansion).

### Tap Target Ergonomics
- Ensure all interactive elements (buttons, links, inputs, icons) have a minimum tap area of 44x44px or adequate surrounding padding to prevent mis-taps.
