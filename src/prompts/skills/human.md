# antislop-human

Human and accessibility skill for antislop: contrast, keyboard, focus, and states for real people.

## Color & Contrast

### Low-Contrast Text
- **Tell:** Light grey text on a white background, thin body text, muted labels chosen because they look "elegant" but are hard to read.
- **Why:** Excludes low-vision users and everyone in bright light.
- **Fix:** Meet WCAG AA minimums: 4.5:1 for normal text, 3:1 for large text (18px+). Compute the ratio; do not eyeball it.

### Text Over a Photo or Gradient
- **Tell:** White text placed directly over an image or gradient that is light in some areas.
- **Why:** Contrast is local. Where the image is light, text drops below 4.5:1.
- **Fix:** Add a scrim or solid backdrop behind text, then verify the worst spot. If any part of the text area fails, the treatment fails.

### The Grey-on-Grey Hallucination
- **Tell:** "Dark grey on black" or "light grey on white" claimed to pass AA without computation (#555555 on black is 2.8:1 - it fails).
- **Fix:** Never assert a pairing passes without checking. Use known safe pairings (e.g. White on #333333 is 12.6:1, Black on white is 21:1).

### Non-Text Contrast
- **Tell:** Buttons, icons, input borders, focus indicators distinguished from background by less than 3:1.
- **Fix:** Give component boundaries and status indicators at least a 3:1 ratio against adjacent colors (WCAG 1.4.11). Pair icons with a text label.

## Keyboard Operability

### Removed Focus Outline
- **Tell:** `outline: none` or `outline: 0` with no replacement focus style.
- **Why:** Keyboard users cannot see where they are.
- **Fix:** Keep or replace with a visible `:focus-visible` style that meets 3:1 contrast against neighbors. Never set `outline: none` without a replacement.

### Mouse-Only Patterns
- **Tell:** Menus opening on hover only, dropdowns that click-open but do not keyboard-open, drag-and-drop with no keyboard fallback.
- **Fix:** Every interactive element must be reachable and operable via keyboard: logical tab order matching visual order, activation with Enter/Space, dialogs closable with Escape.

### Broken Tab Order
- **Tell:** Focus jumps erratically or lands on hidden elements because DOM order does not match visual order.
- **Fix:** Match DOM order to visual layout, add skip links for long pages, and never use `tabindex="-1"` on interactive elements unless part of an intentional modal focus trap.

## Focus & States

### Weak or Invisible Focus Indicator
- **Tell:** Focus ring the same color as background or thinner than 1px.
- **Fix:** Visible focus indicator on every interactive element, 3:1 contrast against adjacent colors, verified in both dark and light modes.

### Color-Only Feedback
- **Tell:** Success, error, or status communicated solely by color (red error text, green border) with no icon or text indicator.
- **Why:** Excludes color-blind users (~8% of men, ~0.5% of women).
- **Fix:** Pair color with an icon, descriptive text label, or shape difference.
