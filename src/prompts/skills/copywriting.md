# Copywriting & Product Messaging Anti-Slop Guidelines

Apply these guidelines when drafting interface copy, technical documentation, READMEs, error messages, and product communications.

## 1. Eliminate Marketing Fluff & Buzzwords
- **Banned hype terms**: Avoid "supercharge", "unleash", "game-changing", "seamlessly", "elevate", "cutting-edge", "next-generation", "empower", "unlock", "pivotal", "delve".
- **State facts over adjectives**: Instead of "blazing fast performance", write "renders in under 16ms" or "processes 10,000 events/sec". Let the engineering metrics speak for themselves.
- **No faux enthusiasm**: Avoid excessive exclamation points, celebratory adjectives ("Welcome aboard!", "Congratulations on creating your first project!"), or forced conversational chitchat.

## 2. Concise, Direct Interface Copy
- **Labels and Buttons**: Use strong, unambiguous action verbs (`Create project`, `Delete API key`, `Export CSV`, `Save changes`) instead of vague phrases (`Proceed`, `Submit`, `Go`, `Click here`).
- **Empty States**: Explain clearly what is missing and provide one concrete next step:
  - *Bad*: "It looks quiet here! You haven't created any workspaces yet. Why not create your first one today?"
  - *Good*: "No workspaces found. Create a workspace to start collaborating."
- **Confirmation Prompts**: Explicitly name the object and consequence of the action:
  - *Bad*: "Are you sure you want to continue?"
  - *Good*: "Delete cluster 'prod-db-1'? This permanently deletes all associated volumes and snapshots."

## 3. Human & Actionable Error Messages
- Always answer three questions in user-facing errors:
  1. What happened?
  2. Why did it happen?
  3. How can the user resolve it?
- *Bad*: "Error 500: Unexpected internal server exception occurred."
- *Good*: "Unable to connect to database host 'db.internal:5432'. Verify network connectivity and credentials in your config."

## 4. Documentation & Technical Writing
- Put the answer or example first. Avoid "throat-clearing" introductory paragraphs ("In today's fast-paced software development landscape...").
- Keep code examples minimal, runnable, and focused directly on the concept. Avoid boilerplate that distracts from the core API.
- Use second person ("you") or imperative mood ("run `cargo build`") rather than passive voice ("the command should be run").
