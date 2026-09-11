# antislop-code

Code comment hygiene for AI coding agents: remove generic AI-slop comments, keep the valuable ones, never touch executable code.

## Comments to Remove

### Decorative Separators
- Eliminate repeated characters or banner boxes:
  - `// =======================`
  - `// -------- WORKFLOW --------`
  - `/* ---- ROUTES ---- */`
- Replace with a clean single line or remove entirely if the label adds no value.

### Restating the Obvious
- Remove comments repeating what syntax already states:
  - `let count = 0; // initialize count`
  - `// User struct` above `struct User`
  - `// Validate user` above `fn validate_user()`

### Workflow Step Narration
- Remove procedural step counting:
  - `// Step 1: Validate input`
  - `// Step 2: Process request`
  - `// Step 3: Return response`
- Control flow is already visible in code.

### Empty Category Labels & Vague Placeholders
- Remove labels without concrete facts: `// Core logic`, `// Helper function`, `// Error handling`.
- Remove speculative or vague placeholders: `// TODO: improve this`, `// Add more validation`. Keep TODOs only when citing a concrete actionable issue or ticket.

### Decorative Emojis & End Markers
- Remove decorative emojis in code: `// ✅ Validation`, `// 🚀 Performance`.
- Remove closing brace comments: `} // end if`, `// end match`.

## Comments to Preserve (Keep Tight: 1–2 Lines)
Never remove or discourage comments explaining:
- Business logic rules and constraints
- Architectural decisions and trade-offs
- Concurrency, memory safety, and thread invariants
- Workarounds for platform, compiler, or library bugs
- Security considerations
- Complex algorithms and non-obvious domain math
