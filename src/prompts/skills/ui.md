# antislop-ui

UI and visual skill for antislop: color, layout, components, and decoration.

## Visual & Color Discipline

### Eliminate AI Cliché Gradients
- Avoid default blue-to-purple (`#6366f1` / `#8b5cf6`), blue-to-cyan, or rainbow gradients.
- Keep gradients strictly as purposeful hierarchy accents, not full-page or full-card defaults.
- Avoid blurry radial orbs or colored light blobs behind hero sections.

### Glassmorphism & Shadow Restraint
- Cap backdrop-blur/glass effects to 1–2 overlay surfaces (e.g. sticky header or modal). Do not frost every card and sidebar.
- Elevation: components sit flat by default. Shadows serve only as functional elevation markers for modals, floating tooltips, or elevated menus.

### Border Radii & Borders
- Avoid uniform pill-shaped (`rounded-full`, `rounded-3xl`) everything.
- Modulate border radius based on container size (smaller items like tags/inputs get tighter radii).
- Avoid low-contrast or glowing border accents around every container.

### Restrained Color Palette
- Limit active palette to 2–3 core colors + 1 accent. Neutral backgrounds make deliberate accent CTAs stand out.

## Layout & Components

### Escape Cookie-Cutter Templates
- Avoid robotic section cadences: hero -> 3 feature cards -> bento grid -> testimonials -> pricing table -> FAQ -> footer. Structure pages around actual user flows and content.
- Vary card presentation: not every feature needs an identical card with an identical top-left icon.
- Bento grids: use only when displayed content inherently varies in aspect ratio and data density.
